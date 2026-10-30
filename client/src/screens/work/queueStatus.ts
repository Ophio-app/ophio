// SPDX-License-Identifier: Apache-2.0
import type { QueueView, Run, Task } from "../../protocol/types";

/** A pending queue record alone must not override a run that has started. */
export function unstartedQueueItem(task: Task, runs: Run[], queue?: QueueView) {
  if (task.state !== "accepted" || task.current_run || runs.some(r => r.task_id === task.task_id)) return undefined;
  return queue?.items.find(i => i.task_id === task.task_id && i.state.state !== "started");
}

/** Where an accepted task waits, read from the same queue item as its
 * conversation and composer: only a task Next up holds is "In Next up". */
export function acceptedLabel(task: Task, runs: Run[], queue?: QueueView): string {
  return unstartedQueueItem(task, runs, queue) ? "In Next up" : queue ? "Waiting to start · not in Next up" : "Waiting to start";
}
