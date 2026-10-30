// SPDX-License-Identifier: Apache-2.0
// The outbox exists because the WebView's saved state may not reach the disk
// before the app is killed. Saved state that lost a send can still hold the
// draft's earlier, settled attempt: the send in the outbox is newer, so it
// must come back as unknown rather than be dropped.
import { afterEach, beforeEach, expect, it } from "vitest";
import type { App } from "./context";
import { keepOutbox } from "./outbox";
import { UiStore } from "./ui";
import type { ControlLink, LinkEvent, Platform, SendOutbox } from "../host/link";
import { HostSession } from "../host/session";
import { allGrants } from "../dev/sim/host";
import { capabilities } from "../dev/sim/scenario";
import type { ClientFrame, Command } from "../protocol/types";
import { send } from "../screens/talk/send";

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
    return this.frames.flatMap((f) => (f.type === "command" ? [f] : []));
  }
  authenticate() {
    this.listener?.({ type: "authenticated",
      challenge: { selected: { major: 1, minor: 0 }, host_id: "host", boot_epoch: "boot", server_nonce: "test" },
      auth: { host_id: "host", device_id: "phone", grants: allGrants, capabilities: capabilities(), companion_version: "test", journal_head: 0 },
    });
  }
  refuse() {
    const frame = this.commands().at(-1)!;
    this.listener?.({ type: "frame", frame: { type: "receipt", command_id: frame.command_id, status: "rejected", seq: null, result: null, error: { code: "conflict", message: "Not now" } } });
  }
}

/** Files on disk: what is written survives the app being killed. */
class Disk implements SendOutbox {
  files = new Map<string, string>();
  async put(profileId: string, commandId: string, record: string) {
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
beforeEach(() => localStorage.clear());
afterEach(() => {
  for (const stop of stops.splice(0)) stop();
  for (const session of sessions.splice(0)) session.stop();
});

function setup(disk: Disk, profileId: string) {
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

const text = "Keep this request";
const request = {
  projectId: "project",
  draftTaskId: "task",
  text,
  annotationId: null,
  label: "Send",
  build: (): Command => ({ command: "send_message", task_id: "task", text, bundle: null, delivery: "steer" }),
};
const settled = () => new Promise((r) => setTimeout(r, 0));

it("restores a newer send the saved state lost, though the draft kept an older attempt", async () => {
  const disk = new Disk();
  const profileId = "outbox-stale";
  const first = setup(disk, profileId);
  first.app.ui.saveDraft("project", "task", { text });

  // An earlier send was refused; that state reached the disk.
  const refused = send(first.app, request, () => {});
  await settled();
  first.link.refuse();
  expect((await refused).ok).toBe(false);
  expect(first.app.ui.draft("project", "task")?.attempt?.phase).toBe("rejected");
  first.app.ui.persistNow();
  const onDisk = localStorage.getItem(`ui.${profileId}`)!;

  // The person sends again; the computer may act on it. The app is killed
  // before the answer, and the WebView's newer saved state never reached disk.
  void send(first.app, request, () => {});
  await settled();
  const [, again] = first.link.commands();
  expect(again).toBeDefined();
  for (const stop of stops.splice(0)) stop();
  localStorage.setItem(`ui.${profileId}`, onDisk);

  const second = setup(disk, profileId);
  await settled();
  // The newer send is what the phone must ask about, not the refused one.
  expect(second.app.ui.draft("project", "task")).toMatchObject({
    text,
    attempt: { phase: "unknown", commandId: again!.command_id, body: again!.body },
  });
  expect([...disk.files.keys()]).toEqual([`${profileId}/${again!.command_id}`]);
  expect(second.link.commands()).toHaveLength(0);
});
