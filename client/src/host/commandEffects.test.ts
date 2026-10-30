// SPDX-License-Identifier: Apache-2.0
// Regression cases for command effects and recovery.
import { afterEach, describe, expect, it } from "vitest";
import type { ControlLink, LinkEvent } from "./link";
import { HostSession } from "./session";
import type {
  ClientFrame,
  CommandReceipt,
  Event,
  TimelineItem,
} from "../protocol/types";
import {
  applyEvent,
  applyGap,
  applyTimelinePage,
  emptyHostState,
} from "../state/projection";

class ReviewLink implements ControlLink {
  sent: ClientFrame[] = [];
  listener: ((event: LinkEvent) => void) | undefined;
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
}

const sessions: HostSession[] = [];
afterEach(() => {
  for (const s of sessions.splice(0)) s.stop();
});
const event = (seq: number, body: Event) => ({
  seq,
  at: seq,
  source: { kind: "companion" as const },
  provenance: "native" as const,
  event: body,
});
const note = (seq: number): TimelineItem => ({
  seq,
  at: seq,
  run_id: null,
  item: { kind: "note", text: `Note ${seq}` },
});

describe("effects and recovery", () => {
  it("keeps admission distinct from later effects", async () => {
    const link = new ReviewLink(),
      session = new HostSession("review", link);
    sessions.push(session);
    session.start();
    const pending = session.command({
      command: "capture_target",
      target_id: "display",
    });
    const command = link.sent.find((f) => f.type === "command");
    if (!command || command.type !== "command")
      throw new Error("Command was not sent");
    const receipt: CommandReceipt = {
      command_id: command.command_id,
      status: "accepted",
      seq: 1,
      result: null,
      error: null,
    };
    link.emit({ type: "frame", frame: { type: "receipt", ...receipt } });
    expect(await pending).toEqual(receipt);
    expect(session.commands.get()[command.command_id]?.phase).toBe("accepted");
  });

  it("retains the asynchronous effect result and typed failure", () => {
    const result = {
      kind: "created" as const,
      path: "notes.md",
      revision: "sha256",
    };
    const failure = {
      code: "outcome_unknown" as const,
      message: "Replacement could not be confirmed",
    };
    const state = applyEvent(
      emptyHostState,
      event(1, {
        type: "command_progress",
        command_id: "save",
        stage: "uncertain",
        detail: failure.message,
        result,
        error: failure,
      }),
      1,
    );
    expect(state.commandStages.save).toMatchObject({ result, error: failure });
  });

  it("does not strand a timeline hole behind the oldest loaded item", async () => {
    const link = new ReviewLink(),
      session = new HostSession("review", link);
    sessions.push(session);
    session.start();
    let state = applyTimelinePage(
      emptyHostState,
      "task",
      [note(1), note(2), note(3)],
      false,
    );
    state = applyGap(state, 4, 200, 200);
    // The task timeline still retains all notes, but the journal no longer does.
    state = applyTimelinePage(
      state,
      "task",
      Array.from({ length: 80 }, (_, i) => note(i + 21)),
      true,
    );
    session.state.set(state);
    const release = session.watchTimeline("task");
    const loading = session.loadTimeline("task", true);
    const query = link.sent.find((f) => f.type === "query");
    if (
      !query ||
      query.type !== "query" ||
      query.query.kind !== "task_timeline"
    )
      throw new Error("No timeline query");
    const before = query.query.before_seq ?? 101;
    const page = Array.from({ length: 100 }, (_, i) => note(i + 1))
      .filter((n) => n.seq < before)
      .slice(-80);
    link.emit({
      type: "frame",
      frame: {
        type: "query_result",
        query_id: query.query_id,
        result: { Ok: { kind: "task_timeline", items: page, has_more: false } },
      },
    });
    await loading;
    const timeline = session.state.get().timelines.task;
    expect(timeline?.items.some((i) => i.seq === 10)).toBe(true);
    expect(timeline?.items).toHaveLength(100);
    release();
  });

  it("keeps items that arrived live after a gap, and extends a whole timeline", () => {
    let state = applyTimelinePage(emptyHostState, "task", [note(1), note(2)], false);
    state = applyGap(state, 3, 50, 50);
    state = applyEvent(
      state,
      event(60, { type: "timeline_appended", task_id: "task", item: note(60) }),
      60,
    );
    state = applyTimelinePage(state, "task", [note(40), note(41)], true);
    expect(state.timelines.task?.items.map((i) => i.seq)).toEqual([40, 41, 60]);
    state = applyTimelinePage(state, "task", [note(38), note(39)], false, true);
    expect(state.timelines.task?.items.map((i) => i.seq)).toEqual([
      38, 39, 40, 41, 60,
    ]);
    expect(state.timelines.task?.hasMore).toBe(false);
  });

  it("ignores a repeated journal event without replaying an effect", () => {
    const env = event(1, {
      type: "command_progress",
      command_id: "c",
      stage: "host_accepted",
      detail: null,
    });
    const once = applyEvent(emptyHostState, env, 1);
    expect(applyEvent(once, env, 2)).toBe(once);
  });
});
