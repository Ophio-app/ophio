// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { Event, EventEnvelope, TimelineItem } from "../protocol/types";
import {
  applyEvent, applyLive, applyReplay, applyTimelinePage, emptyHostState,
  INACTIVE_TIMELINE_ITEMS, INACTIVE_TIMELINES, releaseTimeline, retainTimeline,
  type ProjectionFrame,
} from "./projection";

const item = (seq: number): TimelineItem => ({ seq, at: seq, run_id: "run",
  item: { kind: "activity", tool: "test", summary: "Working", status: "succeeded", detail: "x".repeat(2048) } });
const event = (seq: number, body: Event): EventEnvelope => ({ seq, at: seq, source: { kind: "companion" }, provenance: "native", event: body });
const appended = (seq: number, taskId = "task", value = item(seq)): ProjectionFrame & { env: EventEnvelope } => ({
  env: event(seq, { type: "timeline_appended", task_id: taskId, item: value }), now: seq + 100,
});

it("bounds both the number and size of unopened conversations receiving live events", () => {
  let state = emptyHostState;
  for (let seq = 1; seq <= 10_000; seq++) {
    const frame = appended(seq);
    state = applyEvent(state, frame.env, frame.now);
  }
  expect(state.timelines.task).toMatchObject({ loaded: false, hasMore: true });
  expect(state.timelines.task!.items).toHaveLength(INACTIVE_TIMELINE_ITEMS);
  expect(state.timelines.task!.items[0]!.seq).toBe(10_001 - INACTIVE_TIMELINE_ITEMS);
  for (let seq = 10_001; seq <= 11_000; seq++) {
    const frame = appended(seq, `task-${seq}`);
    state = applyEvent(state, frame.env, frame.now);
  }
  expect(Object.keys(state.timelines)).toHaveLength(INACTIVE_TIMELINES);
  expect(state.timelines["task-11000"]?.items[0]!.seq).toBe(11_000);
});

it.each([false, true])("replays thousands of events with linear item visits (open: %s)", (open) => {
  const visits = (count: number) => {
    let reads = 0;
    // Include delivery updates: replay must not refold the accumulated
    // history for every update, even with the ordered append fast path.
    const tracked = (seq: number): TimelineItem => Object.freeze({ ...item(seq),
      ...(seq > 0 && seq % 50 === 0 ? { item: { kind: "delivery" as const, message_seq: 0, state: "native_delivered" as const } } : {}),
      get seq() { reads++; return seq; } });
    const initial = open
      ? retainTimeline(applyTimelinePage(emptyHostState, "task", [tracked(0)], false), "task")
      : emptyHostState;
    const frames = Array.from({ length: count }, (_, n) => appended(n + 1, "task", tracked(n + 1)));
    reads = 0;
    const next = applyReplay(initial, frames);
    const work = reads;
    expect(next.timelines.task!.items).toHaveLength(open ? count + 1 : INACTIVE_TIMELINE_ITEMS);
    expect(next.timelines.task!.items.at(-1)!.seq).toBe(count);
    expect(next.journalHead).toBe(count);
    expect(initial.timelines.task?.items.length ?? 0).toBe(open ? 1 : 0);
    return work;
  };
  const small = visits(1_000);
  const large = visits(10_000);
  // Counts reads of the real timeline items, avoiding machine-speed thresholds.
  expect(large).toBeLessThan(small * 12);
  expect(large).toBeLessThan(10_000 * 20);
});

it("bounds a replay across thousands of unopened tasks without evicting the open conversation", () => {
  const initial = retainTimeline(applyTimelinePage(emptyHostState, "open", [item(0)], false), "open");
  const frames = Array.from({ length: 10_000 }, (_, n) => appended(n + 1, `task-${n}`));
  const next = applyReplay(initial, frames);
  expect(Object.keys(next.timelines)).toHaveLength(INACTIVE_TIMELINES + 1);
  expect(next.timelines.open).toBe(initial.timelines.open);
  expect(next.timelines["task-9999"]?.items[0]!.seq).toBe(10_000);
});

it("preserves wire-order effects, delivery updates, duplicate suppression and item order", () => {
  const message: TimelineItem = { seq: 1, at: 1, run_id: "run", item: { kind: "user_message", text: "Go", bundle: null, delivery: "queued_for_boundary" } };
  const delivery = (seq: number, state: "host_accepted" | "native_delivered"): TimelineItem => ({ seq, at: seq, run_id: "run", item: { kind: "delivery", message_seq: 1, state } });
  let initial = retainTimeline(applyTimelinePage(emptyHostState, "task", [message], false), "task");
  initial = retainTimeline(initial, "other");
  const delta = (text: string): ProjectionFrame => ({ now: 100, live: { kind: "agent_text_delta", task_id: "task", run_id: "run", message_key: "m", text } });
  const frames: ProjectionFrame[] = [
    delta("Hello"), appended(1, "task", delivery(3, "native_delivered")),
    appended(2, "other"),
    appended(3, "task", { seq: 4, at: 4, run_id: "run", item: { kind: "agent_message", text: "Hello there\n```ophio-outcome\n{}\n```", partial: false } }),
    // Item order is independent of envelope order, and repeated items replace.
    appended(4, "task", delivery(2, "host_accepted")), appended(5, "task", message),
    appended(5, "task", item(99)), appended(4, "task", item(100)),
    delta("Next reply"),
    { env: event(6, { type: "boot_epoch_changed", boot_epoch: "next" }), now: 106 },
    delta("After boot"),
    appended(7, "task", { seq: 5, at: 5, run_id: "run", item: { kind: "run_ended", run_id: "run", state: "completed" } }),
    delta("After run"),
    { env: event(8, { type: "command_progress", command_id: "c", stage: "observed", detail: "Done" }), now: 108 },
  ];
  const expected = frames.reduce((s, f) => "env" in f ? applyEvent(s, f.env, f.now) : applyLive(s, f.live, f.now), initial);
  const next = applyReplay(initial, frames);
  expect(next).toEqual(expected);
  expect(next.timelines.task!.items.map((i) => i.seq)).toEqual([1, 2, 3, 4, 5]);
  expect(next.timelines.task!.items[0]!.item).toMatchObject({ delivery: "native_delivered" });
  expect(next.timelines.task!.items[3]!.item).toMatchObject({ text: "Hello there" });
  expect(message.item).toMatchObject({ delivery: "queued_for_boundary" });
  expect(Object.values(next.liveText).map((l) => l.text)).toEqual(["After run"]);
});

it("releases paged history to a contiguous tail and restores delivery when older messages are read", () => {
  const message: TimelineItem = { seq: 1, at: 1, run_id: "run", item: { kind: "user_message", text: "Go", bundle: null, delivery: "queued_for_boundary" } };
  const delivered: TimelineItem = { seq: 200, at: 200, run_id: "run", item: { kind: "delivery", message_seq: 1, state: "native_delivered" } };
  const initial = retainTimeline(applyTimelinePage(emptyHostState, "task", [message], false), "task");
  const frames = Array.from({ length: 199 }, (_, n) => appended(n + 2, "task", n === 198 ? delivered : item(n + 2)));
  const open = applyReplay(initial, frames);
  expect(open.timelines.task!.items).toHaveLength(200);
  const closed = releaseTimeline(open, "task");
  expect(closed.timelines.task).toMatchObject({ hasMore: true, loaded: true });
  expect(closed.timelines.task!.items[0]!.seq).toBe(121);
  const older = applyTimelinePage(retainTimeline(closed, "task"), "task", [message, ...Array.from({ length: 119 }, (_, n) => item(n + 2))], false, true);
  expect(older.timelines.task!.items).toHaveLength(200);
  expect(older.timelines.task!.items[0]!.item).toMatchObject({ delivery: "native_delivered" });
  expect(older.timelines.task!.hasMore).toBe(false);
});
