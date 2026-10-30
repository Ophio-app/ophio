// SPDX-License-Identifier: Apache-2.0
// "Send again" on a send whose answer was lost repeats it under its first id.
// When that repeat cannot leave the phone (the computer is not connected), the
// first send may still have arrived: the draft must keep asking about that id,
// never turn into "not sent" and invite a new request that could act twice.
import { afterEach, beforeEach, expect, it } from "vitest";
import type { App } from "../../app/context";
import { UiStore } from "../../app/ui";
import type { ControlLink, LinkEvent, Platform } from "../../host/link";
import { HostSession } from "../../host/session";
import { allGrants } from "../../dev/sim/host";
import { capabilities } from "../../dev/sim/scenario";
import type { ClientFrame } from "../../protocol/types";
import { repeatable, resend, send, settle } from "./send";

class Link implements ControlLink {
  frames: ClientFrame[] = [];
  online = false;
  listener: ((event: LinkEvent) => void) | undefined;
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) {
    if (!this.online) return false;
    this.frames.push(frame);
    return true;
  }
  listen(listener: (event: LinkEvent) => void) {
    this.listener = listener;
    return () => {
      this.listener = undefined;
    };
  }
  commands() {
    return this.frames.flatMap((f) => (f.type === "command" ? [f] : []));
  }
  authenticate() {
    this.online = true;
    this.listener?.({ type: "authenticated",
      challenge: { selected: { major: 1, minor: 0 }, host_id: "h", boot_epoch: "boot", server_nonce: "test" },
      auth: { host_id: "h", device_id: "d", grants: allGrants, capabilities: capabilities(), companion_version: "test", journal_head: 0 },
    });
  }
  drop() {
    this.online = false;
    this.listener?.({ type: "state", state: { state: "offline", since: 0, reason: "Network lost", retryAt: null } });
  }
}

const sessions: HostSession[] = [];
beforeEach(() => localStorage.clear());
afterEach(() => {
  for (const s of sessions.splice(0)) s.stop();
});

function setup() {
  const link = new Link();
  const session = new HostSession("resend-offline", link);
  sessions.push(session);
  session.start();
  link.authenticate();
  const app = {
    session,
    ui: new UiStore("resend-offline"),
    profile: { profileId: "resend-offline", hostId: "h", name: "Computer", deviceId: "d", pairedAt: 0, addresses: [], certSha256: "" },
    profiles: [],
    platform: { kind: "simulated" } as Platform,
    switchTo() {},
    pairAnother() {},
    async rename() {},
    forget: async () => {},
  } satisfies App;
  return { app, link };
}

const request = {
  projectId: "p",
  draftTaskId: "t",
  text: "Run the tests",
  annotationId: null,
  label: "Send",
  build: () => ({ command: "send_message" as const, task_id: "t", text: "Run the tests", bundle: null, delivery: "steer" as const }),
};

it("keeps asking about the first send when Send again can't leave the phone", async () => {
  const { app, link } = setup();
  app.ui.saveDraft("p", "t", { text: "Run the tests" });
  const first = send(app, request, () => {});
  // The computer may have it; the answer is lost with the connection.
  link.drop();
  expect((await first).ok).toBe(false);
  const lost = app.ui.draft("p", "t")!.attempt!;
  expect(lost.phase).toBe("unknown");
  expect(repeatable(lost, app.session)).toBe(true);

  // Still offline, the person taps "Send again" on the notice.
  const again = await resend(app, "p", "t", lost, "Send again");
  expect(again.ok).toBe(false);
  expect(link.commands()).toHaveLength(1);

  // Nothing new left, but the first send is still in doubt: its id is kept,
  // so the phone can still ask about it and a repeat stays deduplicated.
  const kept = app.ui.draft("p", "t")!.attempt!;
  expect(kept).toMatchObject({ phase: "unknown", commandId: lost.commandId });
  expect(repeatable(kept, app.session)).toBe(true);

  // Back online, the computer did have the first send: the draft settles as
  // delivered instead of offering to send the same words as a new request.
  link.authenticate();
  settle(app, "p", "t", kept, { command_id: lost.commandId!, status: "accepted", seq: 1, result: null, error: null });
  expect(app.ui.draft("p", "t")).toBeNull();
});
