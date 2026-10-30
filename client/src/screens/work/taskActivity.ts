// SPDX-License-Identifier: Apache-2.0
import type { Approval, AttentionItem, Run, TargetControlView, Task } from "../../protocol/types";
import { helpersFinishing, isTaskRunning, stillFinishing, openAttention, pendingApprovals, type HostState } from "../../state/projection";
import { handBackReason, handBackTarget } from "../../state/handBack";
import { controlStatus } from "../control/controlStatus";

export function runEnded(run: Run | undefined): boolean {
  return !!run && (run.ended_at !== null || ["completed", "failed", "interrupted", "reconciliation_required"].includes(run.state));
}
/** Transient status belongs only to the task's current, still-running run. */
export function taskActivity(state: HostState, task: Task | undefined) {
  if (!task || !isTaskRunning(state, task)) return undefined;
  const activity = state.activity[task.task_id];
  return activity?.runId === task.current_run ? activity : undefined;
}
export function taskStatus(task: Task, run: Run | undefined, view?: TargetControlView): string | null {
  if (run && helpersFinishing(run)) return "Helpers are still finishing";
  if (run && stillFinishing(run)) return "Waiting for what it started to end";
  if (runEnded(run) || !["accepted", "dispatching", "working", "awaiting_decision"].includes(task.state)) return null;
  return controlStatus(task, run, view);
}

/** A current blocker explains the next action; live activity cannot explain it. */
export function taskAttentionReason(state: HostState, task: Task): string | null {
  return attentionReason(state, task, () => pendingApprovals(state, task.task_id)[0], () => openAttention(state).find(a => a.task_id === task.task_id));
}

/** taskAttentionReason for each of `tasks`, reading the approvals and attention once rather than once per task. */
export function taskAttentionReasons(state: HostState, tasks: Task[]): Record<string, string | null> {
  const approvals = firstPerTask(pendingApprovals(state)), items = firstPerTask(openAttention(state));
  return Object.fromEntries(tasks.map(t => [t.task_id, attentionReason(state, t, () => approvals.get(t.task_id), () => items.get(t.task_id))]));
}

function firstPerTask<T extends { task_id: string | null }>(ordered: T[]): Map<string, T> {
  const first = new Map<string, T>();
  for (const x of ordered) if (x.task_id !== null && !first.has(x.task_id)) first.set(x.task_id, x);
  return first;
}

function attentionReason(state: HostState, task: Task, pendingApproval: () => Approval | undefined, openItem: () => AttentionItem | undefined): string | null {
  const target = handBackTarget(state, task);
  if (target) return handBackReason(target);
  const approval = pendingApproval();
  if (approval) return `${approval.kind === "question" ? "Question waiting" : "Approval needed"} · ${approval.title}`;
  const item = openItem();
  if (item) return item.body || (item.kind === "reconciliation_required" ? "Outcome unknown · Review what happened" : "Waiting for your reply");
  return task.state === "awaiting_decision" ? task.status_line || "Waiting for your reply" : null;
}

/** A row's line without what its state word already says: no "Next up" under
 * In Next up, and "Review what happened" rather than the whole "Outcome
 * unknown · Review what happened" under Outcome unknown. */
export function besideBadge(line: string | null | undefined, badge: string): string | null {
  const text = line?.trim();
  if (!text) return null;
  const word = badge.trim().toLowerCase();
  if (text.toLowerCase().startsWith(`${word} · `)) return text.slice(word.length + 3);
  const bare = (s: string) => s.toLowerCase().replace(/^in /, "").replace(/[.…]+$/, "");
  return bare(text) === bare(badge) || (bare(text).length > 3 && word.includes(bare(text))) ? null : text;
}

/** Tasks that wait for the person ahead of the rest, each group in its own order. */
export function needsYouFirst(tasks: Task[], reasons: Record<string, string | null>): Task[] {
  return [...tasks.filter(t => reasons[t.task_id]), ...tasks.filter(t => !reasons[t.task_id])];
}
