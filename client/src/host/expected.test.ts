// SPDX-License-Identifier: Apache-2.0
// A change the person makes shows at once, over what the computer last said,
// and goes once the computer's own state shows it, or it refuses.
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { HostStateStore } from "../app/screenNames";
import { ids, queue, tasks } from "../dev/sim/scenario";
import type { QueueItem } from "../protocol/types";
import { expectQueue, queueAfter } from "../screens/work/useQueue";
import { emptyHostState, type HostState } from "../state/projection";
import { SETTLE_MS, settle } from "./expected";

beforeEach(() => { vi.useFakeTimers(); });
afterEach(() => { vi.useRealTimers(); });

const task = tasks(Date.now())[0]!;
const archived = (s: HostState) => ({ ...s, tasks: { ...s.tasks, [task.task_id]: { ...s.tasks[task.task_id]!, state: "archived" as const } } });

it("shows what is expected over the computer's state, and keeps the computer's state underneath", () => {
  const store = new HostStateStore({ ...emptyHostState, tasks: { [task.task_id]: task } }, () => "home");
  const seen: string[] = [];
  store.subscribe(() => seen.push(store.get().tasks[task.task_id]!.title));
  const drop = store.expect(archived);
  expect(store.get().tasks[task.task_id]!.state).toBe("archived");
  expect(store.base().tasks[task.task_id]!.state).toBe(task.state);
  // A report from the computer is shown with the expected change still over it.
  store.set((s) => ({ ...s, tasks: { ...s.tasks, [task.task_id]: { ...task, title: "Renamed" } } }));
  expect(store.get().tasks[task.task_id]).toMatchObject({ title: "Renamed", state: "archived" });
  expect(store.base().tasks[task.task_id]).toMatchObject({ title: "Renamed", state: task.state });
  drop();
  expect(store.get().tasks[task.task_id]!.state).toBe(task.state);
  expect(seen).toEqual([task.title, "Renamed", "Renamed"]);
  drop();
  expect(seen).toHaveLength(3);
});

it("lets an expected change go once the computer shows it, or after a while if it never does", () => {
  const store = new HostStateStore({ ...emptyHostState, tasks: { [task.task_id]: task } }, () => "home");
  const isArchived = (s: HostState) => s.tasks[task.task_id]?.state === "archived";
  const drop = vi.fn(store.expect(archived));
  settle(store, drop, isArchived);
  vi.advanceTimersByTime(SETTLE_MS - 1);
  expect(drop).not.toHaveBeenCalled();
  store.set(archived);
  expect(drop).toHaveBeenCalledTimes(1);
  expect(store.get()).toBe(store.base());

  const late = vi.fn(store.expect((s) => ({ ...s, tasks: {} })));
  settle(store, late, () => false);
  vi.advanceTimersByTime(SETTLE_MS);
  expect(late).toHaveBeenCalledTimes(1);
  expect(store.get().tasks[task.task_id]).toBeTruthy();
  // Already shown: it goes straight away.
  const shown = vi.fn();
  settle(store, shown, isArchived);
  expect(shown).toHaveBeenCalledTimes(1);
});

const item = (n: number, more: Partial<QueueItem> = {}): QueueItem => ({
  ...queue(0).items[0]!, item_id: `i${n}`, task_id: `t${n}`, position: n, authorization: "auto_start", authorization_expires_at: 99, ...more,
});
const three = () => ({ ...queue(0), items: [item(0, { state: { state: "started", run_id: "r" } }), item(1), item(2)] });

it("orders Next up as the computer will once a move or removal is done", () => {
  const moved = queueAfter(three(), { command: "queue_move", item_id: "i2", position: 1 })!;
  expect(moved.items.map((i) => [i.item_id, i.position])).toEqual([["i0", 0], ["i2", 1], ["i1", 2]]);
  // What has started keeps its permission; everything waiting asks again.
  expect(moved.items.map((i) => i.authorization)).toEqual(["auto_start", "ask_before_start", "ask_before_start"]);
  expect(moved.items[0]!.authorization_expires_at).toBe(99);
  expect(moved.items[1]).not.toHaveProperty("authorization_expires_at");
  // A removal asks again only of what waited on it.
  const removed = queueAfter(three(), { command: "queue_remove", item_id: "i1" })!;
  expect(removed.items.map((i) => [i.item_id, i.position, i.authorization])).toEqual([["i0", 0, "auto_start"], ["i2", 1, "auto_start"]]);
  const chained = { ...three(), items: [...three().items, item(3, { depends_on: ["i2"] })] };
  chained.items[2] = { ...chained.items[2]!, depends_on: ["i1"] };
  expect(queueAfter(chained, { command: "queue_remove", item_id: "i1" })!.items.map((i) => [i.item_id, i.authorization]))
    .toEqual([["i0", "auto_start"], ["i2", "ask_before_start"], ["i3", "ask_before_start"]]);
  expect(queueAfter(three(), { command: "queue_move", item_id: "i1", position: 3 })).toBeNull();
  expect(queueAfter(three(), { command: "queue_remove", item_id: "gone" })).toBeNull();
  expect(queueAfter(three(), { command: "queue_pause", project_id: ids.pTide })).toBeNull();
});

it("shows a Next up change only over the queue it was made on, and puts it back when refused", () => {
  const before = three();
  const store = new HostStateStore({ ...emptyHostState, queues: { [ids.pTide]: before } }, () => "home");
  const order = () => store.get().queues[ids.pTide]!.items.map((i) => i.item_id);
  const refused = expectQueue(store, ids.pTide, { command: "queue_remove", item_id: "i1" }, before.revision);
  expect(order()).toEqual(["i0", "i2"]);
  refused.settled(false);
  expect(order()).toEqual(["i0", "i1", "i2"]);

  const taken = expectQueue(store, ids.pTide, { command: "queue_move", item_id: "i2", position: 1 }, before.revision);
  expect(order()).toEqual(["i0", "i2", "i1"]);
  taken.settled(true);
  // The computer's queue moved on some other way: its word stands.
  store.set((s) => ({ ...s, queues: { [ids.pTide]: { ...before, revision: before.revision + 1 } } }));
  expect(order()).toEqual(["i0", "i1", "i2"]);
  expect(store.get()).toBe(store.base());
});

it("shows a change made while another is on its way over the queue that one leaves, until its own lands", () => {
  const before = { ...queue(0), items: [item(0), item(1), item(2)] };
  const store = new HostStateStore({ ...emptyHostState, queues: { [ids.pTide]: before } }, () => "home");
  const order = () => store.get().queues[ids.pTide]!.items.map((i) => i.item_id);
  const up = { command: "queue_move", item_id: "i2", position: 1 } as const;
  const top = { command: "queue_move", item_id: "i2", position: 0 } as const;
  const first = expectQueue(store, ids.pTide, up, before.revision);
  const second = expectQueue(store, ids.pTide, top, before.revision, true);
  expect(order()).toEqual(["i2", "i0", "i1"]);
  // The computer has the first, and its queue shows it before the second goes.
  first.settled(true);
  const once = { ...queueAfter(before, up)!, revision: before.revision + 1 };
  store.set((s) => ({ ...s, queues: { [ids.pTide]: once } }));
  expect(order()).toEqual(["i2", "i0", "i1"]);
  second.sending(once.revision);
  second.settled(true);
  store.set((s) => ({ ...s, queues: { [ids.pTide]: { ...queueAfter(once, top)!, revision: once.revision + 1 } } }));
  expect(order()).toEqual(["i2", "i0", "i1"]);
  expect(store.get()).toBe(store.base());
});
