// SPDX-License-Identifier: Apache-2.0
import type { SimHost } from "./host";
import type { TimelineBody } from "../../protocol/types";
import { ids } from "./scenario";
import { workIds } from "./work";

function append(host: SimHost, taskId: string, runId: string | null, at: number, item: TimelineBody) {
  host.emit({ type: "timeline_appended", task_id: taskId, item: { seq: host.seq + 1, at, run_id: runId, item } });
}

/** A restarted task with an old receipt arriving after the new run's first message. */
export function twoRuns(host: SimHost) {
  const task = host.tasks.find(t => t.task_id === ids.tDash);
  const first = host.runs.find(r => r.run_id === task?.current_run);
  if (!task || !first) throw new Error("Missing conversation fixture");
  const now = Date.now();
  const old = { ...first, state: "interrupted" as const, ended_at: now - 90_000, holds_writer: false };
  const next = { ...first, run_id: "0192f7a1-0000-7000-8000-00000000f902", admitted_at: now - 60_000,
    dispatched_at: now - 59_500, acknowledged_at: now - 59_000, state: "working" as const, ended_at: null, children: [], outcome: null };
  host.runs = [...host.runs.map(r => r.run_id === old.run_id ? old : r), next];
  for (const run of [old, next]) host.emit({ type: "run_upserted", run });
  const current = { ...task, current_run: next.run_id, state: "working" as const, updated_at: now, status_line: "Checking the chart again" };
  host.tasks = host.tasks.map(t => t.task_id === task.task_id ? current : t);
  host.emit({ type: "task_upserted", task: current });
  append(host, task.task_id, old.run_id, old.ended_at, { kind: "run_ended", run_id: old.run_id, state: "interrupted" });
  append(host, task.task_id, next.run_id, next.admitted_at, { kind: "user_message", text: "Continue with the chart check.", bundle: null, delivery: "native_delivered" });
  append(host, task.task_id, next.run_id, next.acknowledged_at, { kind: "run_started", run_id: next.run_id });
  append(host, task.task_id, old.run_id, now - 89_000, { kind: "receipt", receipt: { kind: "take_over", requested_at: now - 90_000,
    summary: "You have control.", facts: [{ label: "Input released", status: "done", detail: null }] } });
  return { taskId: task.task_id, oldRunId: old.run_id, newRunId: next.run_id };
}

export function repeatCompletions(host: SimHost) {
  const task = host.tasks.find(t => t.task_id === workIds.receipt);
  if (!task) throw new Error("Missing completed task");
  for (let i = 0; i < 12; i++) {
    const item = { attention_id: `completion-repeat-${i}`, task_id: task.task_id, run_id: task.current_run, kind: "completed" as const,
      title: "Task finished", body: null, approval_id: null, priority: 5, created_at: Date.now() + i, resolved_at: null };
    host.attention.push(item); host.emit({ type: "attention_upserted", item });
  }
}

export function longConversation(host: SimHost) {
  for (let i = 0; i < 14; i++) append(host, ids.tDash, ids.rDash, Date.now(), { kind: "agent_message", text: `Chart check ${i + 1}: the labels and both latency lines remain visible.`, partial: false });
}

export function queuedRequest(host: SimHost) {
  const task = host.tasks.find(t => t.task_id === workIds.waiting);
  const source = host.journal.map(e => e.event).find(e => e.type === "timeline_appended" && e.item.item.kind === "user_message" && e.item.item.bundle);
  if (!task || source?.type !== "timeline_appended" || source.item.item.kind !== "user_message" || !source.item.item.bundle)
    throw new Error("Missing queued attachment fixture");
  const bundle = { ...structuredClone(source.item.item.bundle), bundle_id: "0192f7a1-0000-7000-8000-00000000f903" };
  append(host, task.task_id, null, Date.now(), { kind: "user_message", text: "Polish the empty states shown in this screenshot.", bundle, delivery: "host_accepted" });
  return { taskId: task.task_id, bundle };
}

export function failHistoryOnce(host: SimHost) {
  const query = host.query.bind(host);
  let failed = false;
  host.query = async request => {
    if (!failed && request.kind === "task_timeline") { failed = true; throw { code: "temporarily_unavailable", message: "Conversation history could not be loaded." }; }
    return query(request);
  };
}

export function delayedDecision(host: SimHost) {
  const task = host.tasks.find(t => t.task_id === workIds.unknown);
  const original = host.runs.find(r => r.task_id === task?.task_id);
  if (!task || !original) throw new Error("Missing uncertain run fixture");
  const ended = Date.now() - 2 * 3_600_000;
  const run = { ...original, admitted_at: ended - 507_000, acknowledged_at: ended - 506_000, ended_at: ended, state: "reconciliation_required" as const,
    children: [{ child_id: "uncertain-helper", native_ref: null, label: "A helper", state: "unknown" as const, started_at: ended - 400_000, ended_at: null, provenance: "native" as const }],
    outcome: { summary: "The note is drafted. The helper’s final result wasn’t reported.", changed_files: [], checks: [], still_active: [], unknowns: [], usage: null } };
  host.runs = host.runs.map(r => r.run_id === run.run_id ? run : r);
  const updated = { ...task, title: "Draft a short status note", current_run: run.run_id, mission: { goal: "Review the checklist", bounds: [], completion_checks: ["Report whether the checklist covers 200% text"], stop_point: null } };
  host.tasks = host.tasks.map(t => t.task_id === task.task_id ? updated : t);
  host.emit({ type: "run_upserted", run }); host.emit({ type: "task_upserted", task: updated });
  append(host, task.task_id, run.run_id, ended, { kind: "run_ended", run_id: run.run_id, state: "reconciliation_required" });
  return task.task_id;
}
