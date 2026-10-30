// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { freshness } from "./freshness";
import type { ControlLink, LinkEvent, LinkState } from "../host/link";
import type { ClientFrame } from "../protocol/types";
import { ANSWER_MS, HostSession } from "../host/session";
import { snapshot } from "../platform/lifecycle.fixtures";
import { allGrants } from "../dev/sim/host";
import { capabilities } from "../dev/sim/scenario";
import { shortAge } from "../design/time";

describe("freshness wording", () => {
  const offline = (retryAt: number | null) =>
    ({ state: "offline", since: 0, reason: "Wi-Fi dropped", retryAt }) as const;

  it("never says 'in now' or 'now ago'", () => {
    const due = freshness(offline(10_002), true, 10_000, 10_000, "studio");
    expect(due.detail).toBe("Wi-Fi dropped. Last heard just now. Trying again now.");
    const later = freshness(offline(40_001), true, 1, 10_001, "studio");
    expect(later.detail).toBe("Wi-Fi dropped. Last heard 10 s ago. Trying again in 30 s.");
    const stale = freshness(offline(5_000), true, 1, 10_000, "studio");
    expect(stale.detail).toContain("Trying again now.");
  });

  it("ends a reason that is already a sentence only once", () => {
    const silent = { state: "offline", since: 0, reason: "Cannot reach the computer. Check its address and private network.", retryAt: null } as const;
    expect(freshness(silent, true, 1, 50_001, "studio").detail).toBe(
      "Cannot reach the computer. Check its address and private network. Last heard 50 s ago.",
    );
  });

  it("never doubles punctuation, whatever the link's reason ends with", () => {
    // The native link's own reasons, a computer's, the platform's and the simulator's.
    const reasons = [
      "Cannot reach the computer. Check its address and private network.",
      "The connection closed before the computer confirmed the operation.",
      "The computer did not answer in time.",
      "The app supports protocol 1.0; the computer supports 2.0. Update the app on this phone from its original source.",
      "The connection could not send this frame.",
      "Can't reach studio",
      "Offline",
      "Terminal not found",
      "Is the computer awake?",
      "Stopped!",
      "Still starting…",
      "The computer said “try later.”",
      "Closed (code 1006)",
      "Closed (code 1006.)",
      "Wi-Fi dropped.  ",
      "",
    ];
    const states: LinkState[] = [
      { state: "idle" },
      { state: "connecting", attempt: 1 },
      { state: "connecting", attempt: 3 },
      { state: "authenticating" },
      { state: "online", since: 0 },
      ...reasons.flatMap((reason): LinkState[] => [
        { state: "offline", since: 0, reason, retryAt: null },
        { state: "offline", since: 0, reason, retryAt: 120_000 },
        { state: "failed", reason: "revoked", detail: reason },
      ]),
    ];
    for (const link of states)
      for (const computer of ["studio", "mac.", "Alex’s Mac"])
        for (const heard of [null, 1, 99_000])
          for (const caughtUp of [true, false]) {
            const { detail } = freshness(link, caughtUp, heard, 100_000, computer);
            expect(detail, JSON.stringify(link)).not.toMatch(/[.!?…]["'”’)\]]*\s*[.!?…]/);
            expect(detail).not.toMatch(/^\s|\s$|\s{2}/);
          }
  });
});

class Link implements ControlLink {
  listener?: (event: LinkEvent) => void;
  sent: ClientFrame[] = [];
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) {
    this.sent.push(frame);
    return true;
  }
  listen(listener: (event: LinkEvent) => void) {
    this.listener = listener;
    return () => {
      this.listener = undefined;
    };
  }
  emit(event: LinkEvent) {
    this.listener?.(event);
  }
  /** Connect, and answer what the session asks for first, so nothing is left unanswered. */
  authenticate() {
    this.emit({ type: "authenticated",
      challenge: { selected: { major: 1, minor: 0 }, host_id: "host", boot_epoch: "boot", server_nonce: "test" },
      auth: { host_id: "host", device_id: "phone", grants: allGrants, capabilities: capabilities(), companion_version: "test", journal_head: 0 },
    });
    for (const f of this.sent.splice(0))
      if (f.type === "query") this.emit({ type: "frame", frame: { type: "query_result", query_id: f.query_id, result: f.query.kind === "snapshot" ? { Ok: { kind: "snapshot", ...snapshot() } } : { Err: { code: "unsupported", message: "Not in this test" } } } });
  }
}

describe("a computer that stops answering", () => {
  const unreachable = "Cannot reach the computer. Check its address and private network.";
  const link = new Link();
  // The fake clock, so the session's timers fire at the times it records.
  const session = new HostSession("silent", link, () => Date.now());
  const shown = () => freshness(session.link.get(), session.state.get().caughtUp, session.state.get().lastHeardAt, Date.now(), "studio", session.unanswered.get());
  const at = (ms: number) => vi.advanceTimersByTime(ms - Date.now());
  afterEach(() => vi.useRealTimers());

  it("is Checking once a ping goes unanswered, until the link gives up, then keeps one reason and one Last heard through every retry", () => {
    vi.useFakeTimers({ now: 0 });
    session.start();
    link.authenticate();
    link.emit({ type: "frame", frame: { type: "caught_up", head: 0 } });
    at(10_000);
    link.emit({ type: "frame", frame: { type: "pong", nonce: 1 } });
    // The ping 15 s after connecting goes unanswered: Live for a few seconds more.
    at(15_000 + ANSWER_MS - 1);
    expect(shown()).toMatchObject({ label: "Live", live: true });
    at(15_000 + ANSWER_MS);
    expect(shown()).toEqual({ label: "Checking…", tone: "neutral", live: false, detail: "Waiting for studio to answer. Last heard 10 s ago." });
    // In doubt, not yet offline, until the native link gives up.
    at(54_000);
    expect(shown()).toMatchObject({ label: "Checking…", live: false });
    // The native link reports the silence, then each retry that times out as
    // the same unreachable computer.
    let retryAt = 55_000;
    for (let attempt = 1; attempt <= 4; attempt++) {
      vi.setSystemTime(retryAt - 3_000);
      link.emit({ type: "state", state: { state: "offline", since: Date.now(), reason: unreachable, retryAt } });
      expect(shown()).toEqual({
        label: "Offline", tone: "warn", live: false,
        detail: `${unreachable} Last heard ${shortAge(Date.now() - 10_000)} ago. Trying again now.`,
      });
      vi.setSystemTime(retryAt);
      link.emit({ type: "state", state: { state: "connecting", attempt } });
      expect(shown().detail).toBe(`${attempt > 1 ? `Attempt ${attempt}. ` : ""}Last heard ${shortAge(Date.now() - 10_000)} ago.`);
      link.emit({ type: "state", state: { state: "authenticating" } });
      expect(shown()).toMatchObject({ label: "Connecting", detail: `Checking studio’s identity. Last heard ${shortAge(Date.now() - 10_000)} ago.` });
      retryAt += 12_000;
    }
    expect(session.state.get().lastHeardAt).toBe(10_000);
  });

  it("is Live again, heard now, once it answers", () => {
    vi.useFakeTimers({ now: 120_000 });
    link.authenticate();
    expect(shown()).toMatchObject({ label: "Catching up", live: false });
    link.emit({ type: "frame", frame: { type: "caught_up", head: 0 } });
    expect(shown()).toMatchObject({ label: "Live", live: true, detail: "Connected to studio." });
    expect(session.state.get().lastHeardAt).toBe(120_000);
    session.stop();
  });
});

describe("an open link that stops answering", () => {
  let session: HostSession;
  let link: Link;
  beforeEach(() => {
    vi.useFakeTimers();
    link = new Link();
    session = new HostSession("quiet", link);
    session.start();
    link.authenticate();
    link.emit({ type: "frame", frame: { type: "caught_up", head: 0 } });
  });
  afterEach(() => { session.stop(); vi.useRealTimers(); vi.restoreAllMocks(); });
  const label = () => freshness(session.link.get(), session.state.get().caughtUp, null, 0, "studio", session.unanswered.get()).label;

  it("is Checking a few seconds after a request goes unanswered, and Live again at the next answer", () => {
    void session.command({ command: "archive_task", task_id: "task" }, { label: "Archive", commandId: "c1" }).catch(() => undefined);
    vi.advanceTimersByTime(ANSWER_MS - 1);
    expect(label()).toBe("Live");
    vi.advanceTimersByTime(1);
    expect(label()).toBe("Checking…");
    link.emit({ type: "frame", frame: { type: "receipt", command_id: "c1", status: "accepted", seq: 1, result: null, error: null } });
    expect(label()).toBe("Live");
  });

  it("still waits on a request asked in the same millisecond as one that was answered", () => {
    void session.command({ command: "archive_task", task_id: "a" }, { label: "Archive", commandId: "c1" }).catch(() => undefined);
    void session.command({ command: "archive_task", task_id: "b" }, { label: "Archive", commandId: "c2" }).catch(() => undefined);
    link.emit({ type: "frame", frame: { type: "receipt", command_id: "c1", status: "accepted", seq: 1, result: null, error: null } });
    vi.advanceTimersByTime(ANSWER_MS);
    expect(label()).toBe("Checking…");
    link.emit({ type: "frame", frame: { type: "receipt", command_id: "c2", status: "accepted", seq: 2, result: null, error: null } });
    expect(label()).toBe("Live");
  });

  it("is Checking as soon as the device loses its network; its return pings an open link and reconnects a dropped one at once", () => {
    const send = vi.spyOn(link, "send");
    const connect = vi.spyOn(link, "connect");
    window.dispatchEvent(new Event("offline"));
    expect(label()).toBe("Checking…");
    window.dispatchEvent(new Event("online"));
    expect(label()).toBe("Live");
    expect(send).toHaveBeenLastCalledWith(expect.objectContaining({ type: "ping" }));
    const ping = (send.mock.lastCall![0] as { nonce: number }).nonce;
    vi.advanceTimersByTime(ANSWER_MS);
    expect(label()).toBe("Checking…");
    link.emit({ type: "frame", frame: { type: "pong", nonce: ping } });
    expect(label()).toBe("Live");
    link.emit({ type: "state", state: { state: "offline", since: 0, reason: "Wi-Fi dropped", retryAt: 30_000 } });
    window.dispatchEvent(new Event("offline"));
    window.dispatchEvent(new Event("online"));
    expect(connect).toHaveBeenCalledTimes(1);
  });
});

it("words the status button's label, tone and liveness from the link alone", () => {
  const links: LinkState[] = [
    { state: "idle" }, { state: "connecting", attempt: 2 }, { state: "authenticating" }, { state: "online", since: 0 },
    { state: "offline", since: 0, reason: "Wi-Fi dropped", retryAt: 90_000 }, { state: "failed", reason: "revoked", detail: "Removed" },
  ];
  for (const link of links) for (const caughtUp of [false, true]) {
    const { label, tone, live } = freshness(link, caughtUp, null, 0, "studio");
    expect(freshness(link, caughtUp, 55_000, 60_000, "studio")).toMatchObject({ label, tone, live });
  }
});
