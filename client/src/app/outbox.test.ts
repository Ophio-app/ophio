// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, renderHook } from "@testing-library/react";
import { createElement } from "react";
import { AppProvider, type App } from "./context";
import { keepOutbox } from "./outbox";
import { UiStore } from "./ui";
import type { ControlLink, LinkEvent, Platform, SendOutbox } from "../host/link";
import { HostSession } from "../host/session";
import { allGrants } from "../dev/sim/host";
import { capabilities } from "../dev/sim/scenario";
import { snapshot } from "../platform/lifecycle.fixtures";
import type { ClientFrame, Command, QueryData, QueryResult } from "../protocol/types";
import { check, repeatable, resend, send, settle, useAutoSettle } from "../screens/talk/send";

class Link implements ControlLink {
  frames: ClientFrame[] = [];
  listener?: (event: LinkEvent) => void;
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) {
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
    return this.frames.filter((f) => f.type === "command");
  }
  authenticate(epoch = "outbox-boot", grants = allGrants) {
    this.listener?.({ type: "authenticated",
      challenge: { selected: { major: 1, minor: 0 }, host_id: "host", boot_epoch: epoch, server_nonce: "test" },
      auth: { host_id: "host", device_id: "phone", grants, capabilities: capabilities(), companion_version: "test", journal_head: 0 },
    });
  }
  accept() {
    const frame = this.commands().at(-1);
    if (!frame || frame.type !== "command") throw new Error("No command sent");
    this.listener?.({ type: "frame", frame: { type: "receipt", command_id: frame.command_id, status: "accepted", seq: 1, result: null, error: null } });
  }
  answer(data: QueryData) {
    const query = this.frames.findLast(f => f.type === "query" && f.query.kind === data.kind);
    if (query?.type !== "query") throw new Error(`No ${data.kind} query`);
    this.listener?.({ type: "frame", frame: { type: "query_result", query_id: query.query_id, result: { Ok: data } } });
  }
  status(id: string, result: QueryResult["result"]) {
    const query = this.frames.findLast(f => f.type === "query" && f.query.kind === "command_status" && f.query.command_id === id);
    if (query?.type !== "query") throw new Error(`No status query for ${id}`);
    this.listener?.({ type: "frame", frame: { type: "query_result", query_id: query.query_id, result } });
  }
  drop() {
    this.listener?.({ type: "state", state: { state: "offline", since: 0, reason: "Lost answer", retryAt: null } });
  }
}

/** Files on disk: what is written survives the app being killed. */
class Disk implements SendOutbox {
  files = new Map<string, string>();
  refuse = false;
  async put(profileId: string, commandId: string, record: string) {
    if (this.refuse) throw new Error("disk full");
    this.files.set(`${profileId}/${commandId}`, record);
  }
  async list(profileId: string) {
    return [...this.files].filter(([k]) => k.startsWith(`${profileId}/`)).map(([, v]) => v);
  }
  async remove(profileId: string, commandId: string) {
    this.files.delete(`${profileId}/${commandId}`);
  }
}

const sessions: HostSession[] = [];
const stops: (() => void)[] = [];
let serial = 0;
beforeEach(() => localStorage.clear());
afterEach(() => {
  cleanup();
  for (const stop of stops.splice(0)) stop();
  for (const session of sessions.splice(0)) session.stop();
});

function setup(disk: Disk, profileId = `outbox-${++serial}`) {
  const link = new Link();
  const session = new HostSession(profileId, link);
  sessions.push(session);
  session.start();
  link.authenticate();
  const ui = new UiStore(profileId);
  const app: App = {
    session,
    ui,
    profile: { profileId, hostId: "host", deviceId: "phone", name: "Computer", addresses: [], certSha256: "", pairedAt: 0 },
    profiles: [],
    platform: { kind: "native", outbox: disk } as unknown as Platform,
    switchTo() {},
    pairAnother() {},
    async rename() {},
    forget: async () => {},
  };
  stops.push(keepOutbox(disk, profileId, "Computer", ui));
  return { app, link };
}

const request = {
  projectId: "project",
  draftTaskId: "task",
  text: "Keep this request",
  annotationId: null,
  label: "Send",
  build: (): Command => ({ command: "send_message", task_id: "task", text: "Keep this request", bundle: null, delivery: "steer" }),
};

const settled = () => new Promise((r) => setTimeout(r, 0));

describe("send outbox", () => {
  it("keeps a withheld send's identity through grant removal and recovers it when access returns", async () => {
    const disk = new Disk();
    const { app, link } = setup(disk);
    link.answer({ kind: "snapshot", ...snapshot() });
    await settled();
    app.ui.saveDraft("project", "task", { text: request.text });
    const sending = send(app, request, () => {});
    await settled();
    const first = link.commands()[0]!;
    // The computer accepts both commands, but their receipts and progress
    // never reach the phone. The second command keeps its Library permission.
    const other = app.session.command({ command: "library_create", root_id: "root", path: "note.md", text: "note" }, { label: "Create note" }).catch(() => undefined);
    const second = link.commands()[1]!;
    link.drop();
    await sending;
    await other;
    const attempt = app.ui.draft("project", "task")!.attempt!;
    renderHook(() => useAutoSettle("project", "task", attempt), {
      wrapper: ({ children }) => createElement(AppProvider, { app, children }),
    });

    link.authenticate("outbox-boot", { ...allGrants, admin: false, tasks: false });
    await settled();
    const denied = { Err: { code: "permission_denied" as const, message: "Access to this command's status is not permitted" } };
    link.status(first.command_id, denied);
    await settled();
    expect(app.session.commands.get()[first.command_id]?.phase).toBe("unknown");
    expect(app.ui.draft("project", "task")).toMatchObject({ text: request.text, attempt: { phase: "unknown", commandId: first.command_id, body: first.body } });
    expect(JSON.parse([...disk.files.values()][0]!).attempt.commandId).toBe(first.command_id);

    // A denied lookup must not stop recovery of the following command.
    const accepted = (id: string) => ({ Ok: { kind: "command_status" as const,
      receipt: { command_id: id, status: "accepted" as const, seq: 1, result: null, error: null } } });
    link.status(second.command_id, accepted(second.command_id));
    await settled();
    expect(app.session.commands.get()[second.command_id]?.phase).toBe("accepted");

    // The explicit Check path must preserve the outbox on the same denial.
    const checking = check(app, "project", "task", attempt);
    const refused = expect(checking).rejects.toMatchObject({ error: denied.Err });
    link.status(first.command_id, denied);
    await refused;
    expect(app.ui.draft("project", "task")?.attempt?.commandId).toBe(first.command_id);
    expect(disk.files.size).toBe(1);

    link.drop();
    link.authenticate();
    await settled();
    link.status(first.command_id, accepted(first.command_id));
    await settled();
    expect(app.session.commands.get()[first.command_id]?.phase).toBe("accepted");
    expect(app.ui.draft("project", "task")).toBeNull();
    expect(disk.files.size).toBe(0);
    // Recovery settles the original send; it does not send another message.
    expect(link.commands().filter(f => f.body.command === "send_message")).toEqual([first]);
  });

  it("writes the send to disk before the command leaves and removes it once answered", async () => {
    const disk = new Disk();
    const { app, link } = setup(disk);
    app.ui.saveDraft("project", "task", { text: request.text });
    const sending = send(app, request, () => {});
    expect(link.commands()).toHaveLength(0);
    await settled();
    const [frame] = link.commands();
    expect(frame?.type === "command" && JSON.parse([...disk.files.values()][0] ?? "{}").attempt.commandId).toBe(frame?.type === "command" && frame.command_id);
    link.accept();
    expect((await sending).ok).toBe(true);
    await settled();
    expect(disk.files.size).toBe(0);
  });

  it("sends nothing when this device cannot record the send", async () => {
    const disk = new Disk();
    disk.refuse = true;
    const { app, link } = setup(disk);
    app.ui.saveDraft("project", "task", { text: request.text });
    const outcome = await send(app, request, () => {});
    expect(link.commands()).toHaveLength(0);
    expect(outcome.ok).toBe(false);
    expect(app.ui.draft("project", "task")).toMatchObject({ text: request.text, attempt: { phase: "failed", commandId: null } });
  });

  it("restores a send the app was killed during, even if its saved state was lost", async () => {
    const disk = new Disk();
    const first = setup(disk);
    const profileId = first.app.profile.profileId;
    first.app.ui.saveDraft("project", "task", { text: request.text });
    void send(first.app, request, () => {});
    await settled();
    const [frame] = first.link.commands();
    if (frame?.type !== "command") throw new Error("No command sent");
    // Killed before an answer, and the WebView's storage never reached disk.
    for (const stop of stops.splice(0)) stop();
    localStorage.clear();
    const second = setup(disk, profileId);
    await settled();
    expect(second.app.ui.draft("project", "task")).toMatchObject({
      text: request.text,
      attempt: { phase: "unknown", commandId: frame.command_id, body: frame.body },
    });
    expect(second.link.commands()).toHaveLength(0);
    expect(disk.files.size).toBe(1);
  });

  it.each(["same boot", "new boot", "legacy key"])("restores retry safety from the outbox with %s", async (change) => {
    const disk = new Disk();
    const first = setup(disk);
    const profileId = first.app.profile.profileId;
    first.app.ui.saveDraft("project", "task", { text: request.text });
    const sent = send(first.app, request, () => {});
    await settled();
    first.app.session.stop();
    await sent;
    for (const stop of stops.splice(0)) stop();
    // Lose the UI draft but retain the journal baseline, isolating boot trust.
    const baseline = localStorage.getItem(`command-history.${profileId}`)!;
    localStorage.clear();
    localStorage.setItem(`command-history.${profileId}`, baseline);
    if (change === "legacy key") {
      for (const [key, text] of disk.files) {
        const record = JSON.parse(text);
        record.attempt.historyId = JSON.parse(baseline).id;
        disk.files.set(key, JSON.stringify(record));
      }
    }
    const second = setup(disk, profileId);
    if (change === "new boot") second.link.authenticate("next-boot");
    await settled();
    const attempt = second.app.ui.draft("project", "task")!.attempt!;
    expect(repeatable(attempt, second.app.session)).toBe(change === "same boot");
    settle(second.app, "project", "task", attempt, null);
    expect(second.app.ui.draft("project", "task")?.attempt?.phase).toBe(change === "same boot" ? "not_delivered" : "unknown");
    expect(second.link.commands()).toHaveLength(0);
    await settled();
    expect(disk.files.size).toBe(change === "same boot" ? 0 : 1);
  });

  it("does not repeat if the computer restarts while the outbox write is pending", async () => {
    const disk = new Disk();
    const { app, link } = setup(disk);
    app.ui.saveDraft("project", "task", { text: request.text });
    const first = send(app, request, () => {});
    await settled();
    link.listener?.({ type: "state", state: { state: "offline", since: 0, reason: "Lost answer", retryAt: null } });
    await first;
    link.authenticate();
    const attempt = app.ui.draft("project", "task")!.attempt!;
    expect(repeatable(attempt, app.session)).toBe(true);
    let finish!: () => void;
    vi.spyOn(disk, "put").mockImplementation(() => new Promise<void>(resolve => { finish = resolve; }));
    const again = resend(app, "project", "task", attempt, "Repeat");
    link.authenticate("next-boot");
    finish();
    await expect(again).rejects.toThrow("can’t be repeated safely");
    expect(link.commands()).toHaveLength(1);
    expect(app.ui.draft("project", "task")?.attempt).toMatchObject({ phase: "unknown", commandId: attempt.commandId });
    expect(app.ui.draft("project", "task")?.attempt?.detail).toContain("Computer restarted");
    expect(disk.files.size).toBe(1);
  });
});
