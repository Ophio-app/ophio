// SPDX-License-Identifier: Apache-2.0
import type { ItemStatus, Task } from "../protocol/types";

/**
 * Where an issue or pull request stands with the task working it:
 * `item_status` in `crates/services/src/github.rs`. Read from the task as it
 * is now, so a list read a minute ago still shows the work as it goes.
 */
export function itemStatus(task: Task): ItemStatus | null {
  const stage = task.github?.stage;
  if (stage === "ready" || stage === "merging") return "green";
  if (stage === "capped" || stage === "not_reproduced") return "waiting_for_you";
  if (task.state === "accepted" && !task.current_run) return "queued";
  if (task.state === "interrupted") return "stopped";
  if (task.state === "awaiting_decision" || task.state === "failed" || task.state === "reconciliation_required") return "waiting_for_you";
  if (task.attention_count > 0) return "waiting_for_you";
  if (task.state === "dispatching" || task.state === "working" || stage === "babysitting" || stage === "checking") return "agent_working";
  return null;
}

/** A task linked to GitHub whose runs only read and report: a review, a Sort or a check. */
export const writesNothing = (task: Task): boolean => ["find", "triage", "check"].includes(task.github?.link.type ?? "");

/** Whether a task is still on its way: waiting to start, starting or working. */
export const going = (task: Task | undefined): boolean => !!task && ["accepted", "dispatching", "working"].includes(task.state);

/**
 * The newest task working on issue `number`, one that changes it before one
 * that only reads it: `linked_task` in `crates/services/src/github.rs`.
 */
export function linkedTask(tasks: Task[], number: number): Task | undefined {
  return tasks
    .filter((t) => t.state !== "archived" && t.github?.issue === number)
    .sort((a, b) => Number(writesNothing(a)) - Number(writesNothing(b)) || b.created_at - a.created_at)[0];
}
