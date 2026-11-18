// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, useUi, type App } from "../../app/context";
import { draftKey, UiStore, type Route } from "../../app/ui";
import { allGrants } from "../../dev/sim/host";
import { capabilities } from "../../dev/sim/scenario";
import * as notices from "../../design/Announcer";
import * as toasts from "../../design/Toast";
import type { ControlLink, LinkEvent, Platform } from "../../host/link";
import { HostSession } from "../../host/session";
import type { ClientFrame, Command, CommandReceipt } from "../../protocol/types";
import { annotationStore, type SourceDraft } from "../../state/annotations";
import { uuidv7 } from "../../protocol/ids";
import { stillAt } from "../annotate/sources";
import { check, resend, send, settle, useAutoSettle, useSendBusy } from "./send";

vi.mock("../../app/checkWriters", () => ({ useCheckWriters: () => {} }));
vi.mock("../annotate/sources", async (original) => ({ ...(await original<typeof import("../annotate/sources")>()), stillAt: vi.fn() }));

class Link implements ControlLink {
  frames: ClientFrame[] = [];
  listener?: (event: LinkEvent) => void;
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) { this.frames.push(frame); return true; }
  listen(listener: (event: LinkEvent) => void) { this.listener = listener; return () => { this.listener = undefined; }; }
  commands() { return this.frames.filter((frame) => frame.type === "command"); }
  receipt(taskId?: string): CommandReceipt {
    return { command_id: this.commands().at(-1)!.command_id, status: "accepted", seq: 1,
      result: taskId ? { kind: "task_created", task_id: taskId, queue_item_id: null } : null, error: null };
  }
  accept(taskId?: string) { this.listener?.({ type: "frame", frame: { type: "receipt", ...this.receipt(taskId) } }); }
  drop() { this.listener?.({ type: "state", state: { state: "offline", since: 0, reason: "Lost link", retryAt: null } }); }
}

let serial = 0;
const apps: App[] = [];
beforeEach(() => { localStorage.clear(); });
afterEach(() => {
  cleanup();
  for (const app of apps.splice(0)) {
    app.session.stop();
    for (const side of app.ui.navigationStores()) side.retire();
  }
  vi.restoreAllMocks();
});

function setup() {
  const profileId = `shared-send-${++serial}`;
  const link = new Link();
  const session = new HostSession(profileId, link);
  session.start();
  link.listener?.({ type: "authenticated",
    challenge: { selected: { major: 1, minor: 0 }, host_id: "h", boot_epoch: "boot", server_nonce: "test" },
    auth: { host_id: "h", device_id: "d", grants: allGrants, capabilities: capabilities(), companion_version: "test", journal_head: 0 },
  });
  const app: App = {
    session, ui: new UiStore(profileId),
    profile: { profileId, hostId: "h", name: "Computer", deviceId: "d", pairedAt: 0, addresses: [], certSha256: "" },
    profiles: [], platform: { kind: "simulated", haptic: vi.fn() } as unknown as Platform,
    switchTo() {}, pairAnother() {}, async rename() {}, async forget() {},
  };
  apps.push(app);
  const right = { ...app, ui: app.ui.fork("right") };
  return { app, right, link };
}

const request = (text = "Run the tests") => ({
  projectId: "p", draftTaskId: "t" as string | null, text, annotationId: null as string | null, label: "Send",
  build: (): Command => ({ command: "send_message", task_id: "t", text, bundle: null, delivery: "steer" }),
});

function Busy({ name }: { name: string }) {
  const busy = useSendBusy("p", "t");
  return <button disabled={busy !== null}>{name}</button>;
}

function Watcher() {
  const attempt = useUi((state) => state.drafts[draftKey("p", "t")]?.attempt ?? null);
  useAutoSettle("p", "t", attempt);
  return null;
}

it("guards one shared draft across independent navigation, preserving the other pane's later words", async () => {
  const { app, right, link } = setup();
  right.ui.set((state) => ({ ...state, tab: "work" }));
  const navigation = right.ui.get().stacks;
  expect(right.ui).not.toBe(app.ui);
  expect(right.ui.sharedStore()).toBe(app.ui);
  app.ui.saveDraft("p", "t", { text: "Run the tests" });
  const clear = vi.spyOn(app.ui, "clearDraft");
  const first = send(app, request(), () => {});
  const second = send(right, request(), () => {});
  expect(link.commands()).toHaveLength(1);
  right.ui.saveDraft("p", "t", { text: "Run the tests\nThen check the linter" });
  link.accept();
  const outcomes = await Promise.all([first, second]);
  expect(outcomes.every((outcome) => outcome.ok)).toBe(true);
  expect(outcomes[1]?.joined).toBe(true);
  expect(app.ui.draft("p", "t")?.text).toBe("Then check the linter");
  expect(right.ui.draft("p", "t")).toBe(app.ui.draft("p", "t"));
  expect(app.platform.haptic).toHaveBeenCalledTimes(1);
  expect(clear).not.toHaveBeenCalled();
  expect(right.ui.get().tab).toBe("work");
  expect(right.ui.get().stacks).toBe(navigation);
});

it("locks both panes before asynchronous markup preparation and keeps edits made in the second pane", async () => {
  const { app, right, link } = setup();
  const store = annotationStore(app.profile.profileId);
  const { id, createdAt } = store.create("p", "t");
  const video: SourceDraft = {
    key: "S1", blobKey: null, artifactId: "clip", sha256: "a".repeat(64), mediaType: "video/webm", width: 640, height: 360,
    durationMs: 4000, included: false, origin: { kind: "shown", artifact_id: "clip" }, capturedAt: 1, derivedFrom: null, derivation: null,
  };
  store.record(id, (draft) => ({ ...draft, sources: [video], nextSource: 2, selectedSource: "S1",
    watch: { S1: [{ clock: 0, atMs: 0, playing: true, rate: 1, watching: true }] },
    comments: [{ comment_id: uuidv7(createdAt + 1500), text: "It flashes", mark_ids: [], origin: { kind: "typed" }, link_origins: [] }],
  }));
  app.ui.saveDraft("p", "t", { text: "Run the tests", annotationId: id });
  let prepared!: () => void;
  vi.mocked(stillAt).mockImplementation((annotations, annotation, source, atMs) => new Promise((resolve) => {
    prepared = () => {
      const frame: SourceDraft = { ...source, key: "S2", artifactId: "frame", sha256: "b".repeat(64), included: true,
        mediaType: "image/png", durationMs: null, derivedFrom: "S1", derivation: { kind: "frame_from_clip", at_ms: atMs } };
      annotations.place(annotation, (draft) => ({ ...draft, sources: [...draft.sources, frame], nextSource: 3 }));
      resolve(frame);
    };
  }));
  render(<><AppProvider app={app}><Busy name="Left send" /></AppProvider><AppProvider app={right}><Busy name="Right send" /></AppProvider></>);
  const req = { ...request(), annotationId: id };
  let first!: ReturnType<typeof send>, second!: ReturnType<typeof send>;
  await act(async () => {
    first = send(app, req, () => {});
    second = send(right, req, () => {});
  });
  expect(link.commands()).toHaveLength(0);
  expect(stillAt).toHaveBeenCalledTimes(1);
  expect((screen.getByRole("button", { name: "Left send" }) as HTMLButtonElement).disabled).toBe(true);
  expect((screen.getByRole("button", { name: "Right send" }) as HTMLButtonElement).disabled).toBe(true);
  right.ui.saveDraft("p", "t", { text: "A replacement written on the right", files: [{
    key: "later-file", name: "later.txt", size: 10, mediaType: "text/plain", blobKey: "later-blob",
    artifactId: "later-artifact", sha256: "c".repeat(64),
  }] });
  await act(async () => { prepared(); });
  await vi.waitFor(() => expect(link.commands()).toHaveLength(1));
  await act(async () => { link.accept(); await Promise.all([first, second]); });
  expect(app.ui.draft("p", "t")?.text).toBe("A replacement written on the right");
  expect(right.ui.draft("p", "t")?.files?.map((file) => file.key)).toEqual(["later-file"]);
  expect((screen.getByRole("button", { name: "Right send" }) as HTMLButtonElement).disabled).toBe(false);
});

it("coalesces simultaneous exact retries, with the original command body and identity", async () => {
  const { app, right, link } = setup();
  app.ui.saveDraft("p", "t", { text: "Run the tests" });
  const first = send(app, request(), () => {});
  const original = link.commands()[0]!;
  link.drop();
  const outcome = await first;
  if (outcome.ok) throw new Error("Expected an unknown outcome");
  app.session.link.set({ state: "online", since: 0 });
  right.ui.saveDraft("p", "t", { text: "Run the tests\nA later request" });
  const again = resend(right, "p", "t", outcome.attempt, "Retry");
  const also = resend(app, "p", "t", outcome.attempt, "Retry");
  expect(link.commands()).toHaveLength(2);
  expect(link.commands()[1]).toMatchObject({ command_id: original.command_id, body: original.body, expected_revision: original.expected_revision });
  link.accept();
  await Promise.all([again, also]);
  expect(app.ui.draft("p", "t")?.text).toBe("A later request");
});

it.each([false, true])("shares one restored receipt query and confirms once, keeping later edits (%s)", async (edited) => {
  const { app, right } = setup();
  const announce = vi.spyOn(notices, "announce");
  const toast = vi.spyOn(toasts, "toast").mockImplementation(() => () => {});
  const clear = vi.spyOn(app.ui, "clearDraft");
  app.ui.saveDraft("p", "t", { text: "Run the tests", attempt: {
    phase: "unknown", at: 0, commandId: "restored", historyId: app.session.commandHistory,
    text: "Run the tests", detail: "Waiting", body: request().build(),
  } });
  const attempt = app.ui.draft("p", "t")!.attempt!;
  let answer!: (result: { kind: "command_status"; receipt: CommandReceipt }) => void;
  const pending = new Promise<{ kind: "command_status"; receipt: CommandReceipt }>((resolve) => { answer = resolve; });
  const query = vi.spyOn(app.session, "query").mockImplementation(() => pending as never);
  const journalSubscription = vi.spyOn(app.session.state, "subscribe");
  render(<><AppProvider app={app}><Watcher /></AppProvider><AppProvider app={right}><Watcher /></AppProvider></>);
  expect(query).toHaveBeenCalledTimes(1);
  expect(journalSubscription).toHaveBeenCalledTimes(1);
  if (edited) right.ui.saveDraft("p", "t", { text: "Run the tests\nRun the tests\nA later request" });
  const receipt: CommandReceipt = { command_id: "restored", status: "accepted", seq: 1, result: null, error: null };
  await act(async () => { answer({ kind: "command_status", receipt }); await pending; });
  settle(right, "p", "t", attempt, receipt);
  expect(clear).toHaveBeenCalledTimes(edited ? 0 : 1);
  if (edited) expect(app.ui.draft("p", "t")?.text).toBe("Run the tests\nA later request");
  else expect(app.ui.draft("p", "t")).toBeNull();
  expect(right.ui.draft("p", "t")).toBe(app.ui.draft("p", "t"));
  expect(announce.mock.calls.length + toast.mock.calls.length).toBe(1);
});

it("retains one pair of journal and command subscriptions while either pane is mounted", async () => {
  const { app, right, link } = setup();
  app.ui.saveDraft("p", "t", { text: "Run the tests" });
  const first = send(app, request(), () => {});
  link.drop();
  await first;
  const journalSubscription = vi.spyOn(app.session.state, "subscribe");
  const commandSubscription = vi.spyOn(app.session.commands, "subscribe");
  const left = render(<AppProvider app={app}><Watcher /></AppProvider>);
  const afterLeft = journalSubscription.mock.calls.length;
  render(<AppProvider app={right}><Watcher /></AppProvider>);
  expect(journalSubscription.mock.calls.length).toBe(afterLeft);
  expect(journalSubscription).toHaveBeenCalledTimes(1);
  expect(commandSubscription).toHaveBeenCalledTimes(1);
  left.unmount();
  const unknown = app.ui.draft("p", "t")!.attempt!;
  await act(async () => { settle(right, "p", "t", unknown, link.receipt()); });
  expect(app.ui.draft("p", "t")).toBeNull();
});

it.each([[false, false], [false, true], [true, false], [true, true]])("follows a created task only in its origin or matching Review (review=%s, late=%s)", async (matchingReview, late) => {
  const { app, right, link } = setup();
  const route: Route = { screen: "review", projectId: "p", taskId: null, intent: "new_task" };
  app.ui.selectProject("p");
  right.ui.selectProject("p");
  app.ui.push(route, "talk");
  if (matchingReview) right.ui.push(route, "talk");
  const untouched = right.ui.get();
  app.ui.saveDraft("p", null, { text: "A new task" });
  const first = send(app, { ...request("A new task"), draftTaskId: null,
    build: (): Command => ({ command: "create_task", project_id: "p", kind: "coding", mode: "work", title: null,
      prompt: "A new task", bundle: null, harness: "claude_code", model: null, effort: null, mission: null, target_id: null,
      placement: { mode: "start_now" } }),
  }, () => {});
  if (late) {
    const receipt = link.receipt("created");
    link.drop();
    const outcome = await first;
    if (outcome.ok) throw new Error("Expected an unknown send");
    settle(right, "p", null, outcome.attempt, receipt);
  } else {
    link.accept("created");
    await first;
  }
  expect(app.ui.get().taskByProject.p).toBe("created");
  expect(app.ui.get().stacks.talk).toHaveLength(1);
  if (matchingReview) {
    expect(right.ui.get().taskByProject.p).toBe("created");
    expect(right.ui.get().stacks.talk).toHaveLength(1);
  } else {
    expect(right.ui.get().taskByProject).toEqual(untouched.taskByProject);
    expect(right.ui.get().stacks).toEqual(untouched.stacks);
  }
});

it("consumes once when a pending receipt query confirms during an exact retry", async () => {
  const { app, right, link } = setup();
  const toast = vi.spyOn(toasts, "toast").mockImplementation(() => () => {});
  app.ui.saveDraft("p", "t", { text: "Run the tests" });
  const first = send(app, request(), () => {});
  link.drop();
  const unknown = await first;
  if (unknown.ok) throw new Error("Expected an unknown send");
  app.session.link.set({ state: "online", since: 0 });
  const repeat = resend(app, "p", "t", unknown.attempt, "Retry");
  right.ui.saveDraft("p", "t", { text: "Run the tests\nRun the tests\nA later request" });
  settle(right, "p", "t", unknown.attempt, link.receipt());
  link.accept();
  expect((await repeat).ok).toBe(true);
  expect(app.ui.draft("p", "t")?.text).toBe("Run the tests\nA later request");
  expect(toast).toHaveBeenCalledTimes(1);
});

it("joins simultaneous manual receipt checks without letting a stalled automatic query block Check", async () => {
  const { app, right } = setup();
  app.ui.saveDraft("p", "t", { text: "Run the tests", attempt: {
    phase: "unknown", at: 0, commandId: "manual", historyId: app.session.commandHistory,
    text: "Run the tests", detail: "Waiting", body: request().build(),
  } });
  const attempt = app.ui.draft("p", "t")!.attempt!;
  let answer!: (result: { kind: "command_status"; receipt: null }) => void;
  const pending = new Promise<{ kind: "command_status"; receipt: null }>((resolve) => { answer = resolve; });
  const query = vi.spyOn(app.session, "query")
    .mockImplementationOnce(() => new Promise<never>(() => {}))
    .mockImplementation(() => pending as never);
  render(<><AppProvider app={app}><Watcher /></AppProvider><AppProvider app={right}><Watcher /></AppProvider></>);
  expect(query).toHaveBeenCalledTimes(1);
  const first = check(app, "p", "t", attempt);
  const second = check(right, "p", "t", attempt);
  expect(query).toHaveBeenCalledTimes(2);
  await act(async () => { answer({ kind: "command_status", receipt: null }); await Promise.all([first, second]); });
  expect(app.ui.draft("p", "t")?.attempt?.phase).toBe("not_delivered");
});
