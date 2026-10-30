// SPDX-License-Identifier: Apache-2.0
//
// Opening the app after the computer has done a lot shows where it is now,
// in one step, not every state it passed through on the way: a project's
// count of queued tasks goes straight from 14 to none instead of counting down.
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { allGrants } from "../dev/sim/host";
import * as fx from "../dev/sim/scenario";
import { snapshot } from "../platform/lifecycle.fixtures";
import type { ClientFrame, QueryData, ServerFrame, TimelineItem } from "../protocol/types";
import { applyTimelinePage, INACTIVE_TIMELINE_ITEMS, INACTIVE_TIMELINES, liveTextForTask } from "../state/projection";
import type { ControlLink, LinkEvent } from "./link";
import { HostSession } from "./session";

class ScriptedLink implements ControlLink {
  sent: ClientFrame[] = [];
  #listener: ((e: LinkEvent) => void) | undefined;
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) {
    this.sent.push(frame);
    return true;
  }
  listen(listener: (e: LinkEvent) => void) {
    this.#listener = listener;
    return () => { this.#listener = undefined; };
  }
  emit(e: LinkEvent) {
    this.#listener?.(e);
  }
  frame(frame: ServerFrame) {
    this.emit({ type: "frame", frame });
  }
  answer(data: QueryData) {
    const query = this.sent.findLast((f) => f.type === "query" && f.query.kind === data.kind);
    if (query?.type !== "query") throw new Error(`No ${data.kind} query`);
    this.frame({ type: "query_result", query_id: query.query_id, result: { Ok: data } });
  }
  authenticate(head = 0) {
    const snap = snapshot();
    this.emit({ type: "authenticated", challenge: { selected: { major: 1, minor: 0 }, host_id: snap.host.host_id, boot_epoch: fx.ids.boot, server_nonce: "fixture" },
      auth: { host_id: snap.host.host_id, device_id: fx.ids.device, grants: allGrants, capabilities: fx.capabilities(), companion_version: "test", journal_head: head } });
  }
  lastSubscribe() {
    const f = this.sent.findLast((f) => f.type === "subscribe");
    return f?.type === "subscribe" ? f.after_seq : undefined;
  }
  lastPing() {
    const f = this.sent.findLast((f) => f.type === "ping");
    return f?.type === "ping" ? f.nonce : undefined;
  }
}

const flush = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };
const task = () => snapshot().tasks[0]!;
/** The computer's journal entry for the task's title at step `seq`. */
const step = (seq: number): ServerFrame => ({ type: "event", seq, at: seq, source: { kind: "device", device_id: fx.ids.device }, provenance: "native",
  event: { type: "task_upserted", task: { ...task(), title: `Step ${seq}` } } });
const delta = (text: string): ServerFrame => ({ type: "live", kind: "agent_text_delta", task_id: task().task_id, run_id: "run-1", message_key: "m1", text });

let link: ScriptedLink;
let session: HostSession;
/** Each title the screen was given, in order. */
let shown: string[];
beforeEach(async () => {
  localStorage.clear();
  link = new ScriptedLink();
  session = new HostSession("catch-up", link);
  session.start();
  link.authenticate();
  link.answer({ kind: "snapshot", ...snapshot() });
  await flush();
  link.frame({ type: "caught_up", head: 0 });
  shown = [];
  let last = session.state.get().tasks[task().task_id]?.title;
  session.state.subscribe(() => {
    const title = session.state.get().tasks[task().task_id]?.title;
    if (title !== last) shown.push(title ?? "");
    last = title;
  });
});
afterEach(() => {
  session.stop();
  vi.useRealTimers();
});

it("what happened while the phone was away is shown in one step once the computer has caught up", async () => {
  link.emit({ type: "state", state: { state: "offline", since: 1, reason: "Connection interrupted", retryAt: null } });
  link.authenticate(14);
  await flush();
  expect(link.lastSubscribe()).toBe(0);
  for (let seq = 1; seq <= 14; seq++) link.frame(step(seq));
  expect(shown).toEqual([]);
  expect(session.state.get().journalHead).toBe(0);
  link.frame({ type: "caught_up", head: 14 });
  expect(shown).toEqual(["Step 14"]);
  expect(session.state.get()).toMatchObject({ journalHead: 14, caughtUp: true });
  // From then on each change shows as it happens.
  link.frame(step(15));
  expect(shown).toEqual(["Step 14", "Step 15"]);
});

it("a subscription the computer closes mid-replay resumes after what arrived", () => {
  link.frame({ type: "subscription_closed", after_seq: 0, reason: { code: "limit_exceeded", message: "event consumer fell behind" } });
  link.frame(step(1));
  link.frame(step(2));
  expect(shown).toEqual([]);
  link.frame({ type: "subscription_closed", after_seq: 2, reason: { code: "limit_exceeded", message: "event consumer fell behind" } });
  expect(link.lastSubscribe()).toBe(2);
  expect(shown).toEqual(["Step 2"]);
  link.frame(step(3));
  link.frame({ type: "caught_up", head: 3 });
  expect(shown).toEqual(["Step 2", "Step 3"]);
});

it("what arrives as the app comes back to the front is shown once the computer answers", () => {
  document.dispatchEvent(new Event("visibilitychange"));
  const nonce = link.lastPing();
  expect(nonce).toBeDefined();
  for (let seq = 1; seq <= 14; seq++) link.frame(step(seq));
  link.frame(delta("Hello"));
  expect(shown).toEqual([]);
  link.frame({ type: "pong", nonce: nonce! });
  expect(shown).toEqual(["Step 14"]);
  expect(liveTextForTask(session.state.get(), task().task_id).map((l) => l.text)).toEqual(["Hello"]);
  link.frame(step(15));
  expect(shown).toEqual(["Step 14", "Step 15"]);
});

it("an unanswered ping holds nothing back for long", () => {
  vi.useFakeTimers();
  session.foreground();
  link.frame(step(1));
  link.frame(step(2));
  expect(shown).toEqual([]);
  vi.advanceTimersByTime(3_000);
  expect(shown).toEqual(["Step 2"]);
  link.frame(step(3));
  expect(shown).toEqual(["Step 2", "Step 3"]);
});

it("a long replay shows how far it has come while it runs", async () => {
  vi.useFakeTimers();
  link.emit({ type: "state", state: { state: "offline", since: 1, reason: "Connection interrupted", retryAt: null } });
  link.authenticate(40);
  await flush();
  for (let seq = 1; seq <= 20; seq++) link.frame(step(seq));
  vi.advanceTimersByTime(3_000);
  expect(shown).toEqual(["Step 20"]);
  for (let seq = 21; seq <= 40; seq++) link.frame(step(seq));
  expect(shown).toEqual(["Step 20"]);
  link.frame({ type: "caught_up", head: 40 });
  expect(shown).toEqual(["Step 20", "Step 40"]);
});

it("what arrived before the link dropped is kept", async () => {
  link.emit({ type: "state", state: { state: "offline", since: 1, reason: "Connection interrupted", retryAt: null } });
  link.authenticate(5);
  await flush();
  link.frame(step(1));
  link.frame(step(2));
  link.emit({ type: "state", state: { state: "offline", since: 2, reason: "Connection interrupted", retryAt: null } });
  expect(shown).toEqual(["Step 2"]);
  expect(session.state.get().journalHead).toBe(2);
  // The next connection asks only for what came after.
  link.authenticate(5);
  await flush();
  expect(link.lastSubscribe()).toBe(2);
});

const activity = (seq: number): TimelineItem => ({ seq, at: seq, run_id: null,
  item: { kind: "activity", tool: "test", summary: "Working", status: "succeeded", detail: "x".repeat(2048) } });
const timelineEvent = (seq: number, taskId: string, item = activity(seq)): ServerFrame => ({
  type: "event", seq, at: seq, source: { kind: "companion" }, provenance: "native",
  event: { type: "timeline_appended", task_id: taskId, item },
});

it.each([false, true])("merges a 10,000-event session replay once per conversation (open: %s)", async (open) => {
  const taskId = task().task_id;
  const release = open ? session.watchTimeline(taskId) : () => {};
  if (open) session.state.set((s) => applyTimelinePage(s, taskId, [], false));
  link.emit({ type: "state", state: { state: "offline", since: 1, reason: "Connection interrupted", retryAt: null } });
  link.authenticate(10_000);
  await flush();
  let reads = 0;
  for (let seq = 1; seq <= 10_000; seq++) {
    const value: TimelineItem = { ...activity(seq),
      ...(seq % 50 === 0 ? { item: { kind: "delivery" as const, message_seq: 0, state: "native_delivered" as const } } : {}),
      get seq() { reads++; return seq; } };
    link.frame(timelineEvent(seq, taskId, value));
  }
  expect(session.state.get().journalHead).toBe(0);
  link.frame({ type: "caught_up", head: 10_000 });
  expect(reads).toBeLessThan(200_000);
  expect(session.state.get().timelines[taskId]!.items).toHaveLength(open ? 10_000 : INACTIVE_TIMELINE_ITEMS);
  expect(session.state.get()).toMatchObject({ journalHead: 10_000, caughtUp: true });
  release();
  expect(session.state.get().timelines[taskId]!.items).toHaveLength(INACTIVE_TIMELINE_ITEMS);
});

it("protects a page from racing events and cache eviction, then pages back from the trimmed tail", async () => {
  const taskId = task().task_id;
  const release = session.watchTimeline(taskId);
  const opening = session.loadTimeline(taskId);
  // The answer was read before these events, but reaches the phone after them.
  for (let seq = 1; seq <= 200; seq++) link.frame(timelineEvent(seq, taskId));
  for (let seq = 201; seq <= 300; seq++) link.frame(timelineEvent(seq, `other-${seq}`));
  link.answer({ kind: "task_timeline", items: [activity(0)], has_more: false });
  await opening;
  expect(session.state.get().timelines[taskId]!.items.map((i) => i.seq)).toEqual(Array.from({ length: 201 }, (_, n) => n));
  expect(session.state.get().timelineReaders[taskId]).toBe(1);
  release();
  release(); // A screen's cleanup is idempotent.
  expect(session.state.get().timelineReaders).toEqual({});
  expect(Object.keys(session.state.get().timelines)).toHaveLength(INACTIVE_TIMELINES);
  expect(session.state.get().timelines[taskId]!.items[0]!.seq).toBe(121);
  expect(session.state.get().timelines[taskId]!.hasMore).toBe(true);

  const stopReading = session.watchTimeline(taskId);
  const older = session.loadTimeline(taskId, true);
  const query = link.sent.findLast((f) => f.type === "query");
  expect(query).toMatchObject({ query: { kind: "task_timeline", before_seq: 121 } });
  link.answer({ kind: "task_timeline", items: Array.from({ length: 80 }, (_, n) => activity(n + 41)), has_more: true });
  await older;
  expect(session.state.get().timelines[taskId]!.items.map((i) => i.seq)).toEqual(Array.from({ length: 160 }, (_, n) => n + 41));
  stopReading();
});

it("releases the in-flight reader when a history query fails", async () => {
  const taskId = task().task_id;
  const opening = session.loadTimeline(taskId);
  const rejected = expect(opening).rejects.toThrow("Not connected to the computer");
  link.emit({ type: "state", state: { state: "offline", since: 1, reason: "Connection interrupted", retryAt: null } });
  await rejected;
  expect(session.state.get().timelineReaders).toEqual({});
});

it("protects an initial query even without a mounted conversation", async () => {
  const taskId = task().task_id;
  const opening = session.loadTimeline(taskId);
  for (let seq = 1; seq <= 200; seq++) link.frame(timelineEvent(seq, taskId));
  for (let seq = 201; seq <= 300; seq++) link.frame(timelineEvent(seq, `other-${seq}`));
  expect(session.state.get().timelines[taskId]!.items).toHaveLength(200);
  link.answer({ kind: "task_timeline", items: [activity(0)], has_more: false });
  await opening;
  const timeline = session.state.get().timelines[taskId]!;
  expect(timeline.items.map((i) => i.seq)).toEqual(Array.from({ length: 80 }, (_, n) => n + 121));
  expect(timeline).toMatchObject({ hasMore: true, loaded: true });
  expect(session.state.get().timelineReaders).toEqual({});
});
