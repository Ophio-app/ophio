// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { App } from "../../app/context";
import { UiStore } from "../../app/ui";
import type { ControlLink, LinkEvent, Platform } from "../../host/link";
import { HostSession } from "../../host/session";
import { allGrants } from "../../dev/sim/host";
import { capabilities } from "../../dev/sim/scenario";
import type {
  ClientFrame,
  CommandReceipt,
  ReceiptStatus,
} from "../../protocol/types";
import { resolveAgent } from "./intent";
import { projects, harnesses } from "../../dev/sim/scenario";
import { repeatable, resend, send, settle } from "./send";
import { uuidv7 } from "../../protocol/ids";
import { annotationStore, editedSinceSent, type SourceDraft } from "../../state/annotations";
import { stillAt } from "../annotate/sources";

vi.mock("../annotate/sources", async (original) => ({ ...(await original<typeof import("../annotate/sources")>()), stillAt: vi.fn() }));

class Link implements ControlLink {
  frames: ClientFrame[] = [];
  listener: ((event: LinkEvent) => void) | undefined;
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
  answer(status: ReceiptStatus) {
    const frame = this.commands().at(-1);
    if (!frame) throw new Error("No command");
    const receipt: CommandReceipt = {
      command_id: frame.command_id,
      status,
      seq: 1,
      result: null,
      error: null,
    };
    this.listener?.({ type: "frame", frame: { type: "receipt", ...receipt } });
  }
  drop() {
    this.listener?.({
      type: "state",
      state: {
        state: "offline",
        since: 0,
        reason: "Network lost",
        retryAt: null,
      },
    });
  }
}

const sessions: HostSession[] = [];
beforeEach(() => localStorage.clear());
afterEach(() => {
  for (const s of sessions.splice(0)) s.stop();
});

function setup() {
  const link = new Link();
  const session = new HostSession("send-test", link);
  sessions.push(session);
  session.start();
  link.listener?.({ type: "authenticated",
    challenge: { selected: { major: 1, minor: 0 }, host_id: "h", boot_epoch: "send-test-boot", server_nonce: "test" },
    auth: { host_id: "h", device_id: "d", grants: allGrants, capabilities: capabilities(), companion_version: "test", journal_head: 0 },
  });
  const app = {
    session,
    ui: new UiStore("send-test"),
    profile: {
      profileId: "send-test",
      hostId: "h",
      name: "Computer",
      deviceId: "d",
      pairedAt: 0,
      addresses: [],
      certSha256: "",
    },
    profiles: [],
    platform: { kind: "simulated" } as Platform,
    switchTo() {},
    pairAnother() {},
    async rename() {},
    forget: async () => {},
  } satisfies App;
  return { app, link };
}

const request = (text: string) => ({
  projectId: "p",
  draftTaskId: "t",
  text,
  annotationId: null,
  label: "Send",
  build: () => ({
    command: "send_message" as const,
    task_id: "t",
    text,
    bundle: null,
    delivery: "steer" as const,
  }),
});

describe("sending from Chat", () => {
  it("keeps what was typed after the sent text once the computer accepts", async () => {
    const { app, link } = setup();
    app.ui.saveDraft("p", "t", { text: "Run the tests" });
    const out = send(app, request("Run the tests"), () => {});
    app.ui.saveDraft("p", "t", { text: "Run the tests\nthen the linter" });
    link.answer("accepted");
    expect((await out).ok).toBe(true);
    expect(app.ui.draft("p", "t")?.text).toBe("then the linter");
    expect(app.ui.draft("p", "t")?.attempt).toBeNull();
  });

  it("clears the draft when nothing changed during the send", async () => {
    const { app, link } = setup();
    app.ui.saveDraft("p", "t", { text: "Run the tests" });
    const out = send(app, request("Run the tests"), () => {});
    link.answer("accepted");
    await out;
    expect(app.ui.draft("p", "t")).toBeNull();
  });

  it("sends an unknown attempt again under the same id and request", async () => {
    const { app, link } = setup();
    app.ui.saveDraft("p", "t", { text: "Run the tests" });
    const first = send(app, request("Run the tests"), () => {});
    link.drop();
    const out = await first;
    expect(out.ok).toBe(false);
    const attempt = app.ui.draft("p", "t")?.attempt ?? null;
    expect(attempt?.phase).toBe("unknown");
    expect(repeatable(attempt, app.session)).toBe(true);

    app.session.link.set({ state: "online", since: 1 });
    const again = resend(app, "p", "t", attempt!, "Send again");
    link.answer("duplicate");
    expect((await again).ok).toBe(true);
    const [a, b] = link.commands();
    expect(b?.command_id).toBe(a?.command_id);
    expect(b?.body).toEqual(a?.body);
    expect(app.ui.draft("p", "t")).toBeNull();
  });

  it("marks a send still in flight at restart as unknown and keeps its id", () => {
    const { app } = setup();
    app.ui.saveDraft("p", "t", { text: "Run the tests" });
    void send(app, request("Run the tests"), () => {});
    window.dispatchEvent(new Event("pagehide"));
    const attempt = new UiStore("send-test").draft("p", "t")?.attempt;
    expect(attempt?.phase).toBe("unknown");
    expect(attempt?.commandId).toBe(app.ui.draft("p", "t")?.attempt?.commandId);
    expect(repeatable(attempt ?? null, app.session)).toBe(true);
  });

  it("a refusal found later leaves the text and forgets the id", () => {
    const { app } = setup();
    app.ui.saveDraft("p", "t", {
      text: "Run the tests",
      attempt: {
        phase: "unknown",
        at: 0,
        commandId: "c",
        detail: "",
        text: "Run the tests",
        body: request("x").build(),
      },
    });
    const attempt = app.ui.draft("p", "t")!.attempt!;
    settle(app, "p", "t", attempt, {
      command_id: "c",
      status: "rejected",
      seq: null,
      result: null,
      error: { code: "conflict", message: "Changed" },
    });
    const d = app.ui.draft("p", "t");
    expect(d?.text).toBe("Run the tests");
    expect(d?.attempt?.phase).toBe("rejected");
    expect(repeatable(d?.attempt ?? null, app.session)).toBe(false);
  });
});

it.each([false, true])("consumes a task's agent pin, including retained newer text (%s)", async retained => {
  const { app, link } = setup();
  const project = projects(1000)[0]!;
  const pin = { harness: "codex" as const, model: "gpt-6-astra", effort: "medium" };
  app.ui.saveDraft(project.project_id, null, { text: "Queue this task", agent: pin });
  const out = send(app, { ...request("Queue this task"), projectId: project.project_id, draftTaskId: null }, () => {});
  if (retained) app.ui.saveDraft(project.project_id, null, { text: "Queue this task\nA later request" });
  link.answer("accepted");
  expect((await out).ok).toBe(true);
  expect(app.ui.draft(project.project_id, null)?.agent ?? null).toBeNull();
  if (retained) expect(app.ui.draft(project.project_id, null)?.text).toBe("A later request");
  const next = resolveAgent(project, harnesses, app.ui.draft(project.project_id, null)?.agent);
  expect(next?.harness).toBe(project.default_harness);
  const other = { ...projects(1000)[1]!, default_harness: "claude_code" as const, default_model: "claude-opus-5-5", default_effort: "low" };
  app.ui.saveDraft(other.project_id, null, { text: "Check this annotation" });
  expect(resolveAgent(other, harnesses, app.ui.draft(other.project_id, null)?.agent)).toEqual({ harness: other.default_harness, model: other.default_model, effort: other.default_effort, launchPolicy: "bypass_permissions" });
  // Explicitly saved project defaults survive consuming a task's private choice.
  expect(resolveAgent({ ...project, default_harness: pin.harness, default_model: pin.model, default_effort: pin.effort }, harnesses, null)).toEqual({ ...pin, launchPolicy: "bypass_permissions" });
});

it("waits for the still of a comment made over a video before sending it", async () => {
  const { app, link } = setup();
  const store = annotationStore(app.profile.profileId);
  const { id, createdAt } = store.create("p", "t");
  const video: SourceDraft = {
    key: "S1", blobKey: null, artifactId: "shown-clip", sha256: "a".repeat(64), mediaType: "video/webm", width: 640, height: 360,
    durationMs: 4000, included: false, origin: { kind: "shown", artifact_id: "shown-clip" }, capturedAt: 1, derivedFrom: null, derivation: null,
  };
  store.record(id, (d) => ({
    ...d, sources: [video], nextSource: 2, selectedSource: "S1",
    watch: { S1: [{ clock: 0, atMs: 0, playing: true, rate: 1, watching: true }] },
    comments: [{ comment_id: uuidv7(createdAt + 1500), text: "It flashes.", mark_ids: [], origin: { kind: "typed" }, link_origins: [] }],
  }));
  let taken: () => void = () => {};
  vi.mocked(stillAt).mockImplementation((s, annotation, source, atMs) => new Promise((resolve) => {
    taken = () => {
      const frame: SourceDraft = { ...source, key: "S2", artifactId: "frame", sha256: "b".repeat(64), included: true, mediaType: "image/png",
        durationMs: null, derivedFrom: "S1", derivation: { kind: "frame_from_clip", at_ms: atMs } };
      s.place(annotation, (d) => ({ ...d, sources: [...d.sources, frame], nextSource: 3 }));
      resolve(frame);
    };
  }));
  const out = send(app, {
    ...request("About the video"), annotationId: id,
    build: (bundle) => ({ command: "send_message" as const, task_id: "t", text: "About the video", bundle, delivery: "steer" as const }),
  }, () => {});
  for (let i = 0; i < 5; i++) await Promise.resolve();
  expect(link.commands()).toEqual([]);
  taken();
  await vi.waitFor(() => expect(link.commands()).toHaveLength(1));
  link.answer("accepted");
  expect((await out).ok).toBe(true);
  const body = link.commands()[0]!.body;
  expect(body.command === "send_message" && body.bundle?.comments[0]?.moment).toEqual({ still: "S2", start_ms: 1500, end_ms: 1500 });
});

it.each(["edited", "removed"])("sends the markup as it was at the tap while its moments wait, and keeps a comment %s since for the next message", async (since) => {
  const { app, link } = setup();
  const store = annotationStore(app.profile.profileId);
  const { id, createdAt } = store.create("p", "t");
  const video: SourceDraft = {
    key: "S1", blobKey: null, artifactId: "shown-clip", sha256: "a".repeat(64), mediaType: "video/webm", width: 640, height: 360,
    durationMs: 4000, included: false, origin: { kind: "shown", artifact_id: "shown-clip" }, capturedAt: 1, derivedFrom: null, derivation: null,
  };
  store.record(id, (d) => ({
    ...d, sources: [video], nextSource: 2, selectedSource: "S1",
    watch: { S1: [{ clock: 0, atMs: 0, playing: true, rate: 1, watching: true }] },
    comments: [{ comment_id: uuidv7(createdAt + 1500), text: "It flashes.", mark_ids: [], origin: { kind: "typed" }, link_origins: [] }],
  }));
  app.ui.saveDraft("p", "t", { text: "About the video", annotationId: id });
  let taken: () => void = () => {};
  vi.mocked(stillAt).mockImplementation((s, annotation, source, atMs) => new Promise((resolve) => {
    taken = () => {
      const frame: SourceDraft = { ...source, key: "S2", artifactId: "frame", sha256: "b".repeat(64), included: true, mediaType: "image/png",
        durationMs: null, derivedFrom: "S1", derivation: { kind: "frame_from_clip", at_ms: atMs } };
      s.place(annotation, (d) => ({ ...d, sources: [...d.sources, frame], nextSource: 3 }));
      resolve(frame);
    };
  }));
  const out = send(app, {
    ...request("About the video"), annotationId: id,
    build: (bundle) => ({ command: "send_message" as const, task_id: "t", text: "About the video", bundle, delivery: "steer" as const }),
  }, () => {});
  for (let i = 0; i < 5; i++) await Promise.resolve();
  store.update(id, (d) => ({ ...d, comments: since === "edited" ? d.comments.map((c) => ({ ...c, text: "It flickers." })) : [] }));
  taken();
  await vi.waitFor(() => expect(link.commands()).toHaveLength(1));
  link.answer("accepted");
  expect((await out).ok).toBe(true);
  const body = link.commands()[0]!.body;
  const sent = body.command === "send_message" ? body.bundle?.comments[0] : null;
  expect(sent?.text).toBe("It flashes.");
  expect(sent?.moment).toEqual({ still: "S2", start_ms: 1500, end_ms: 1500 });
  // The edit made after the tap is the draft's, with the same moment, and waits for the next message.
  const now = store.get()[id]!;
  if (since === "edited") {
    expect(now.comments[0]?.text).toBe("It flickers.");
    expect(now.comments[0]?.moment).toEqual({ still: "S2", start_ms: 1500, end_ms: 1500 });
  } else expect(now.comments).toEqual([]);
  expect(editedSinceSent(now)).toBe(true);
  expect(app.ui.draft("p", "t")?.annotationId).toBe(id);
});
