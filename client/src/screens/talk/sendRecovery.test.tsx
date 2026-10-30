// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import type { ControlLink, LinkEvent, Platform } from "../../host/link";
import { HostSession } from "../../host/session";
import type {
  AnnotationBundle,
  ClientFrame,
  Command,
  CommandReceipt,
} from "../../protocol/types";
import { annotationStore, editedSinceSent, heldLabels, holdUnanswered, markLabel, nextMarkIndex } from "../../state/annotations";
import { emptyHostState } from "../../state/projection";
import * as fixtures from "../../dev/sim/scenario";
import { allGrants } from "../../dev/sim/host";
import { RouteProvider } from "../../shell/Screen";
import SafetyStrip from "../control/SafetyStrip";
import Review from "./Review";
import { resend, send, settle } from "./send";

class Link implements ControlLink {
  frames: ClientFrame[] = [];
  listener?: (event: LinkEvent) => void;
  /** Set when a frame handed over now can't leave the phone. */
  refuses = false;
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) {
    if (this.refuses) return false;
    this.frames.push(frame);
    return true;
  }
  listen(listener: (event: LinkEvent) => void) {
    this.listener = listener;
    return () => {
      this.listener = undefined;
    };
  }
  command() {
    const frame = this.frames.findLast((f) => f.type === "command");
    if (!frame || frame.type !== "command") throw new Error("No command sent");
    return frame;
  }
  receipt(): CommandReceipt {
    return {
      command_id: this.command().command_id,
      status: "accepted",
      seq: 1,
      result: null,
      error: null,
    };
  }
  accept() {
    this.listener?.({
      type: "frame",
      frame: { type: "receipt", ...this.receipt() },
    });
  }
  drop() {
    this.listener?.({
      type: "state",
      state: {
        state: "offline",
        since: 0,
        reason: "Disconnected",
        retryAt: null,
      },
    });
  }
}

const sessions: HostSession[] = [];
let serial = 0;
beforeEach(() => {
  localStorage.clear();
  vi.useFakeTimers();
});
afterEach(() => {
  cleanup();
  for (const session of sessions.splice(0)) session.stop();
  vi.clearAllTimers();
  vi.useRealTimers();
});

function setup() {
  const profileId = `send-recovery-${++serial}`,
    link = new Link();
  const session = new HostSession(profileId, link);
  sessions.push(session);
  session.start();
  link.listener?.({ type: "authenticated",
    challenge: { selected: { major: 1, minor: 0 }, host_id: "host", boot_epoch: "send-recovery-boot", server_nonce: "test" },
    auth: { host_id: "host", device_id: "phone", grants: allGrants, capabilities: fixtures.capabilities(), companion_version: "test", journal_head: 0 },
  });
  const app: App = {
    session,
    ui: new UiStore(profileId),
    profile: {
      profileId,
      hostId: "host",
      deviceId: "phone",
      name: "Computer",
      addresses: [],
      certSha256: "",
      pairedAt: 0,
    },
    profiles: [],
    platform: { kind: "simulated" } as Platform,
    switchTo() {},
    pairAnother() {},
    async rename() {},
    forget: async () => {},
  };
  return { app, link };
}

const request = {
  projectId: "project",
  draftTaskId: "task",
  text: "Keep this request",
  annotationId: null,
  label: "Send",
  build: (): Command => ({
    command: "send_message",
    task_id: "task",
    text: "Keep this request",
    bundle: null,
    delivery: "steer",
  }),
};

describe("send recovery", () => {
  it("makes the command identity durable before handing it to the link", async () => {
    const { app, link } = setup();
    app.ui.saveDraft("project", "task", { text: request.text });
    vi.advanceTimersByTime(300); // The ordinary draft was already saved.
    const sending = send(app, request, () => {});
    const id = link.command().command_id;
    // A process kill does not deliver pagehide and cancels pending timers.
    const reopened = new UiStore(app.profile.profileId);
    link.accept();
    await sending;
    expect(reopened.draft("project", "task")?.attempt?.commandId).toBe(id);
  });

  it("the primary Review action cannot replace an unresolved command with a new id", () => {
    const { app, link } = setup();
    const project = fixtures
      .projects(Date.now())
      .find((p) => p.project_id === fixtures.ids.pGarden);
    if (!project) throw new Error("Missing project fixture");
    app.session.state.set({
      ...emptyHostState,
      caughtUp: true,
      projects: { [project.project_id]: project },
      harnesses: fixtures.harnesses,
      grants: {
        view: true,
        tasks: true,
        approvals: true,
        control_input: true,
        terminal: true,
        computer_use: true,
        library_read: true,
        library_write: true,
        transfers: true,
        admin: true,
        accounts: false,
      },
    });
    const body: Command = {
      command: "create_task",
      project_id: project.project_id,
      kind: "coding",
      mode: "work",
      title: null,
      prompt: request.text,
      bundle: null,
      harness: "claude_code",
      model: null,
      effort: null,
      mission: null,
      target_id: null,
      placement: { mode: "start_now" },
    };
    const originalId = "01900000-0000-7000-8000-000000000001";
    app.ui.saveDraft(project.project_id, null, {
      text: request.text,
      attempt: {
        phase: "unknown",
        at: 0,
        commandId: originalId,
        text: request.text,
        detail: "The computer has not answered.",
        body,
      },
    });
    const route = {
      screen: "review" as const,
      projectId: project.project_id,
      taskId: null,
      intent: "new_task" as const,
    };
    render(
      <AppProvider app={app}>
        <RouteProvider value={{ tab: "talk", route, depth: 1, active: true }}>
          <Review route={route} />
        </RouteProvider>
      </AppProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: /^Start task$/ }));
    const sent = link.frames.filter((frame) => frame.type === "command");
    expect(sent.every((frame) => frame.command_id === originalId)).toBe(true);
  });

  it("the dedicated retry reuses both command identity and reviewed request", async () => {
    const { app, link } = setup();
    app.ui.saveDraft("project", "task", { text: request.text });
    const first = send(app, request, () => {});
    const original = link.command();
    link.drop();
    const outcome = await first;
    if (outcome.ok) throw new Error("Expected an uncertain send");
    app.session.link.set({ state: "online", since: 0 });
    const retry = resend(app, "project", "task", outcome.attempt, "Send again");
    expect(link.command()).toMatchObject({
      command_id: original.command_id,
      body: original.body,
      expected_revision: original.expected_revision,
    });
    link.accept();
    expect((await retry).ok).toBe(true);
  });

  it("keeps annotation edits made after sending when a restart loses the prepared snapshot", async () => {
    const { app, link } = setup();
    const store = annotationStore(app.profile.profileId);
    const annotation = store.create("project", "task");
    store.update(annotation.id, (d) => ({
      ...d,
      generalNote: "Original note",
    }));
    app.ui.saveDraft("project", "task", {
      text: request.text,
      annotationId: annotation.id,
    });
    const sending = send(
      app,
      {
        ...request,
        annotationId: annotation.id,
        build: (bundle): Command => ({
          command: "send_message",
          task_id: "task",
          text: request.text,
          delivery: "steer",
          bundle,
        }),
      },
      () => {},
    );
    await Promise.resolve();
    const receipt = link.receipt();
    link.drop();
    const outcome = await sending;
    if (outcome.ok) throw new Error("Expected an uncertain send");
    store.update(annotation.id, (d) => ({
      ...d,
      generalNote: "A later unsent correction",
    }));
    window.dispatchEvent(new Event("pagehide"));
    // prepared is process-local; all persistent annotation edits remain.
    store.prepared.clear();
    const reopened = { ...app, ui: new UiStore(app.profile.profileId) };
    settle(reopened, "project", "task", outcome.attempt, receipt);
    expect(reopened.ui.draft("project", "task")?.annotationId).toBe(
      annotation.id,
    );
    const current = store.get()[annotation.id];
    if (!current) throw new Error("Annotation disappeared");
    expect(editedSinceSent(current)).toBe(true);
  });
  it("keeps a sent mark's letter taken when the mark is undone before a restart learns the send was accepted", async () => {
    const { app, link } = setup();
    const store = annotationStore(app.profile.profileId);
    const annotation = store.create("project", "task");
    store.update(annotation.id, (d) => ({
      ...d,
      sources: [{
        key: "S1", blobKey: null, artifactId: "artifact-S1", sha256: "sha-S1", mediaType: "image/png", width: 100, height: 100,
        durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 100, derivedFrom: null, derivation: null,
      }],
      nextSource: 2,
      marks: [{ mark_id: "m1", label: "A", source_key: "S1", shape: { kind: "pin", at: { x: 0.5, y: 0.5, t: null } }, color: 0, created_at: 0, deleted: false }],
    }));
    app.ui.saveDraft("project", "task", { text: request.text, annotationId: annotation.id });
    const sending = send(
      app,
      {
        ...request,
        annotationId: annotation.id,
        build: (bundle): Command => ({ command: "send_message", task_id: "task", text: request.text, delivery: "steer", bundle }),
      },
      () => {},
    );
    await vi.advanceTimersByTimeAsync(0);
    const receipt = link.receipt();
    link.drop();
    const outcome = await sending;
    if (outcome.ok) throw new Error("Expected an uncertain send");
    store.undo(annotation.id);
    expect(store.get()[annotation.id]?.marks).toEqual([]);
    window.dispatchEvent(new Event("pagehide"));
    store.prepared.clear();
    settle({ ...app, ui: new UiStore(app.profile.profileId) }, "project", "task", outcome.attempt, receipt);
    const current = store.get()[annotation.id];
    if (!current) throw new Error("Annotation disappeared");
    expect(markLabel(nextMarkIndex(current))).toBe("B");
    // The confirmation reserves the letter, so the send no longer holds it.
    expect(current.held).toEqual({});
  });
  /** A task's message holding markup with one uploaded image and mark A. */
  function markedUp(app: App) {
    const store = annotationStore(app.profile.profileId);
    const annotation = store.create("project", "task");
    store.update(annotation.id, (d) => ({
      ...d,
      sources: [{
        key: "S1", blobKey: null, artifactId: "artifact-S1", sha256: "sha-S1", mediaType: "image/png", width: 100, height: 100,
        durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 100, derivedFrom: null, derivation: null,
      }],
      nextSource: 2,
      marks: [{ mark_id: "m1", label: "A", source_key: "S1", shape: { kind: "pin", at: { x: 0.5, y: 0.5, t: null } }, color: 0, created_at: 0, deleted: false }],
    }));
    app.ui.saveDraft("project", "task", { text: request.text, annotationId: annotation.id });
    const next = (ui: UiStore) => markLabel(nextMarkIndex(store.get()[annotation.id]!, heldLabels(store, ui, annotation.id)));
    const withMarkup = {
      ...request,
      annotationId: annotation.id,
      build: (bundle: AnnotationBundle | null): Command => ({ command: "send_message", task_id: "task", text: request.text, delivery: "steer", bundle }),
    };
    return { store, annotation, next, withMarkup };
  }
  it("holds a sent mark's letter while nobody knows whether the send arrived, and frees it once it did not", async () => {
    const { app, link } = setup();
    const { store, annotation, next, withMarkup } = markedUp(app);
    const sending = send(app, withMarkup, () => {});
    // Undo A and draw again before the computer answers.
    store.undo(annotation.id);
    expect(next(app.ui)).toBe("B");
    await vi.advanceTimersByTimeAsync(0);
    link.drop();
    const outcome = await sending;
    if (outcome.ok) throw new Error("Expected an uncertain send");
    expect(next(app.ui)).toBe("B");
    // A restart still holds it: the saved attempt says what was sent.
    window.dispatchEvent(new Event("pagehide"));
    store.prepared.clear();
    const restarted = new UiStore(app.profile.profileId);
    expect(next(restarted)).toBe("B");
    // The computer never received it, so A is free again.
    settle({ ...app, ui: restarted }, "project", "task", outcome.attempt, null);
    expect(next(restarted)).toBe("A");
  });
  it("keeps holding an unanswered send's letter when Send anyway replaces it and does not arrive", async () => {
    const { app, link } = setup();
    const { store, annotation, next, withMarkup } = markedUp(app);
    const first = send(app, withMarkup, () => {});
    await vi.advanceTimersByTimeAsync(0);
    link.drop();
    const unknown = await first;
    if (unknown.ok || unknown.attempt.phase !== "unknown") throw new Error("Expected an uncertain send");
    store.undo(annotation.id);
    // Send anyway leaves nothing: the phone is no longer connected. Its
    // attempt takes the place of the first, whose fate is still unknown.
    link.refuses = true;
    const anyway = await send(app, withMarkup, () => {});
    if (anyway.ok) throw new Error("Expected the second send to stay on the phone");
    expect(anyway.attempt.phase).toBe("not_delivered");
    expect(app.ui.draft("project", "task")?.attempt?.commandId).toBeNull();
    expect(next(app.ui)).toBe("B");
    window.dispatchEvent(new Event("pagehide"));
    store.prepared.clear();
    expect(next(new UiStore(app.profile.profileId))).toBe("B");
  });
  it("keeps holding the letter of an unanswered send an earlier version saved when Send anyway replaces it", async () => {
    const { app, link } = setup();
    const { store, annotation, next, withMarkup } = markedUp(app);
    const first = send(app, withMarkup, () => {});
    await vi.advanceTimersByTimeAsync(0);
    link.drop();
    const unknown = await first;
    if (unknown.ok || !unknown.attempt.markup) throw new Error("Expected an uncertain send");
    store.undo(annotation.id);
    // As an earlier version saved it: no hold of its own, and no label
    // index with the markup, whose content still names mark A.
    store.record(annotation.id, ({ held: _held, ...d }) => d);
    const { labels: _labels, ...earlier } = unknown.attempt.markup;
    app.ui.saveDraft("project", "task", { attempt: { ...unknown.attempt, markup: earlier } });
    window.dispatchEvent(new Event("pagehide"));
    store.prepared.clear();
    // The app starts.
    const restarted = new UiStore(app.profile.profileId);
    holdUnanswered(store, restarted);
    link.refuses = true;
    const anyway = await send({ ...app, ui: restarted }, withMarkup, () => {});
    if (anyway.ok) throw new Error("Expected the second send to stay on the phone");
    expect(restarted.draft("project", "task")?.attempt?.commandId).toBeNull();
    expect(next(restarted)).toBe("B");
  });
});

describe("intervention reachability", () => {
  it("keeps one Stop visible and exposes other tasks in the modal safety rail", () => {
    const { app } = setup();
    const base = fixtures.tasks(Date.now())[0];
    if (!base) throw new Error("Missing task fixture");
    const tasks = Array.from({ length: 4 }, (_, i) => ({
      ...base,
      task_id: `task-${i}`,
      title: `Running task ${i + 1}`,
      state: "working" as const,
      mode: "ask" as const,
    }));
    app.session.state.set({
      ...emptyHostState,
      tasks: Object.fromEntries(tasks.map((t) => [t.task_id, t])),
      grants: {
        view: true,
        tasks: true,
        approvals: true,
        control_input: true,
        terminal: true,
        computer_use: true,
        library_read: true,
        library_write: true,
        transfers: true,
        admin: true,
        accounts: false,
      },
    });
    render(
      <AppProvider app={app}>
        <SafetyStrip dismiss={() => {}} />
      </AppProvider>,
    );
    expect(screen.getAllByRole("button", { name: /^Stop/ })).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: /Safety · 4 running tasks/ }));
    expect(screen.getAllByRole("button", { name: /^Stop/ })).toHaveLength(4);
  });
});
