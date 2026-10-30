// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it } from "vitest";
import type { App } from "../app/context";
import { UiStore } from "../app/ui";
import type { ControlLink, LinkEvent, Platform } from "./link";
import { HostSession } from "./session";
import { allGrants } from "../dev/sim/host";
import { capabilities } from "../dev/sim/scenario";
import type { ClientFrame } from "../protocol/types";
import { resend, send } from "../screens/talk/send";

class Link implements ControlLink {
  frames: ClientFrame[] = [];
  listener: ((event: LinkEvent) => void) | undefined;
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) { this.frames.push(frame); return true; }
  listen(listener: (event: LinkEvent) => void) { this.listener = listener; return () => { this.listener = undefined; }; }
  commands() { return this.frames.flatMap((f) => (f.type === "command" ? [f] : [])); }
}
const sessions: HostSession[] = [];
beforeEach(() => localStorage.clear());
afterEach(() => { for (const s of sessions.splice(0)) s.stop(); });

it("a resend the computer answers as a duplicate of its earlier refusal keeps the message and says it was refused", async () => {
  const link = new Link();
  const session = new HostSession("dup-test", link);
  sessions.push(session);
  session.start();
  link.listener?.({ type: "authenticated",
    challenge: { selected: { major: 1, minor: 0 }, host_id: "h", boot_epoch: "dup-boot", server_nonce: "test" },
    auth: { host_id: "h", device_id: "d", grants: allGrants, capabilities: capabilities(), companion_version: "test", journal_head: 0 } });
  const app = { session, ui: new UiStore("dup-test"),
    profile: { profileId: "dup-test", hostId: "h", name: "Computer", deviceId: "d", pairedAt: 0, addresses: [], certSha256: "" },
    profiles: [], platform: { kind: "simulated" } as Platform, switchTo() {}, pairAnother() {}, async rename() {}, forget: async () => {} } satisfies App;
  app.ui.saveDraft("p", "t", { text: "Also check the p99 line" });
  const first = send(app, { projectId: "p", draftTaskId: "t", text: "Also check the p99 line", annotationId: null, label: "Send",
    build: () => ({ command: "send_message" as const, task_id: "t", text: "Also check the p99 line", bundle: null, delivery: "steer" as const }) }, () => {});
  // The computer refused it, but the connection dropped before the receipt arrived.
  link.listener?.({ type: "state", state: { state: "offline", since: 0, reason: "Network lost", retryAt: null } });
  await first;
  const attempt = app.ui.draft("p", "t")!.attempt!;
  expect(attempt.phase).toBe("unknown");
  app.session.link.set({ state: "online", since: 1 });
  const again = resend(app, "p", "t", attempt, "Send again");
  const frame = link.commands().at(-1)!;
  expect(frame.command_id).toBe(attempt.commandId);
  // services/commands.rs `dedupe`: the stored refusal comes back marked Duplicate, its error kept.
  link.listener?.({ type: "frame", frame: { type: "receipt", command_id: frame.command_id, status: "duplicate", seq: null, result: null,
    error: { code: "conflict", message: "Task has no active run" } } });
  const out = await again;
  expect(out.ok).toBe(false);
  expect(app.ui.draft("p", "t")?.text).toBe("Also check the p99 line");
  expect(app.ui.draft("p", "t")?.attempt?.phase).toBe("rejected");
});
