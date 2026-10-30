// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { useRoute } from "../../shell/Screen";
import { useMay } from "../../shell/NotAllowed";
import { type WriterHold, writerHoldState } from "../talk/intent";
import type { HostStateStore } from "../../app/screenNames";
import { settle } from "../../host/expected";
import type { Command, QueueItem, QueueView } from "../../protocol/types";

export function useQueue(projectId: string | null) {
  const { session, profile } = useApp();
  const link = useLink();
  const { active } = useRoute();
  const queue = useHost((s) => (projectId ? s.queues[projectId] : undefined));
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const allowed = useMay("tasks");
  useEffect(() => {
    if (!projectId || !active || !allowed || link.state !== "online") return;
    let alive = true;
    setError(null);
    void session
      .query({ kind: "queue", project_id: projectId })
      .then((r) => {
        if (!alive) return;
        session.state.set((s) =>
          (s.queues[projectId]?.revision ?? -1) > r.queue.revision
            ? s
            : { ...s, queues: { ...s.queues, [projectId]: r.queue } },
        );
      })
      .catch((e) => {
        if (alive) setError(describeError(e, profile.name));
      });
    return () => {
      alive = false;
    };
  }, [projectId, active, allowed, link.state, session, profile.name, retry]);
  return { queue, error, allowed, reload: () => setRetry((n) => n + 1) };
}

/** The same ordered pending items on Work and Next up; history is separate. */
export function pendingQueueItems(queue: import("../../protocol/types").QueueView | undefined) {
  return (queue?.items ?? []).filter((item) => item.state.state !== "started").sort((a, b) => a.position - b.position);
}

/** An item's state in Next up. Only the front item can start, so only it is
 * ever ready; the rest say how many tasks are ahead of them. */
/** @param hold what keeps a Work item from starting though the queue says
 * it may: the computer asks for the go-ahead whatever the writer is doing. */
export function queueItemState(item: import("../../protocol/types").QueueItem, index: number, hold?: WriterHold | null): string {
  if (item.state.state === "blocked") return `Blocked — ${item.state.reason}`;
  if (index > 0) return `After ${index} earlier ${index === 1 ? "task" : "tasks"}`;
  if (hold) return writerHoldState(hold);
  return item.state.state === "awaiting_start" ? "Ready — needs your go-ahead" : "Waiting";
}

/** Next up's state on its header and Work's row: it runs only while nothing
 * at its front waits on the person or on an earlier task. */
export function queueState(queue: import("../../protocol/types").QueueView) {
  const front = pendingQueueItems(queue)[0]?.state.state;
  return queue.paused ? "Paused" : front === "awaiting_start" ? "Needs your go-ahead" : front === "blocked" ? "Blocked" : "Running";
}

export function queueMoveError(queue: import("../../protocol/types").QueueView, itemId: string, position: number): string | null {
  const items = [...queue.items];
  const index = items.findIndex((i) => i.item_id === itemId);
  const item = items[index];
  if (!item || !items[position] || item.state.state === "started" || items[position]?.state.state === "started") return "Choose a waiting position.";
  items.splice(index, 1); items.splice(position, 0, item);
  if (items.some((i, at) => i.state.state !== "started" && i.depends_on.some((id) => items.findIndex((d) => d.item_id === id) >= at))) return "Move refused: each task must stay after the tasks it waits for.";
  return null;
}

/**
 * The ids of `changed` and of everything in `items` that waits on them,
 * directly or through others: what asks again before it starts once they
 * are edited or taken out of Next up (`withdraw` in crates/domain/src/queue.rs).
 */
export function waitingOn(items: readonly QueueItem[], changed: readonly string[]): Set<string> {
  const affected = new Set(changed);
  for (let size = -1; size !== affected.size;) {
    size = affected.size;
    for (const i of items) if (i.depends_on.some((dep) => affected.has(dep))) affected.add(i.item_id);
  }
  return affected;
}

/**
 * Next up as the computer will have it once `body` is done. A move asks
 * again of every item still waiting (`invalidate` in
 * crates/domain/src/queue.rs); a removal only of what waited on the item
 * removed. Null for anything else.
 */
export function queueAfter(queue: QueueView, body: Command): QueueView | null {
  if (body.command !== "queue_move" && body.command !== "queue_remove") return null;
  const index = queue.items.findIndex((i) => i.item_id === body.item_id);
  if (index < 0 || (body.command === "queue_move" && body.position >= queue.items.length)) return null;
  const items = [...queue.items];
  const [item] = items.splice(index, 1);
  if (body.command === "queue_move") items.splice(body.position, 0, item!);
  const asks = body.command === "queue_move" ? null : waitingOn(items, [body.item_id]);
  return {
    ...queue,
    items: items.map((i, position) => {
      if (i.state.state === "started" || (asks && !asks.has(i.item_id))) return i.position === position ? i : { ...i, position };
      const { authorization_expires_at: _, ...rest } = i;
      return { ...rest, position, authorization: "ask_before_start" };
    }),
  };
}

/** A move or removal shown in Next up before the computer has it. */
export interface ExpectedQueueChange {
  /** It goes to the computer against this revision of the queue. */
  sending(revision: number): void;
  /** True once the computer has taken it: it stays until the queue the
   * computer sends shows it. False puts the queue back. */
  settled(ok: boolean): void;
}

/**
 * Shows a move or removal in Next up at once, over the queue it was made
 * on. One made while another change is on its way (`waiting`) goes once
 * that one is answered, so until it is sent it also shows over the queue
 * that one leaves.
 */
export function expectQueue(state: HostStateStore, projectId: string, body: Command, revision: number, waiting = false): ExpectedQueueChange {
  let sent = waiting ? Infinity : revision;
  const drop = state.expect((s) => {
    const queue = s.queues[projectId];
    const after = queue && queue.revision >= revision && queue.revision <= sent ? queueAfter(queue, body) : null;
    return after ? { ...s, queues: { ...s.queues, [projectId]: after } } : s;
  });
  return {
    sending(at) { sent = at; },
    settled: (ok) => (ok ? settle(state, drop, (s) => (s.queues[projectId]?.revision ?? sent) > sent) : drop()),
  };
}
