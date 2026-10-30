// SPDX-License-Identifier: Apache-2.0
import { openAttention, type HostState } from "../../state/projection";
import type { AttentionItem } from "../../protocol/types";
import { RUN_END } from "../talk/timeline";
import { latestRun, runMode } from "./logic";

/** Group repeated task updates while retaining every distinct decision. */
export function attentionGroups(items: AttentionItem[]): AttentionItem[][] {
  const groups = new Map<string, AttentionItem[]>();
  for (const item of items) {
    const key = item.task_id ? `${item.task_id}/${item.child_id ?? ""}/${item.kind}` : item.attention_id;
    const group = groups.get(key) ?? [];
    group.push(item);
    groups.set(key, group);
  }
  return [...groups.values()].map(group => group.sort((a, b) => b.created_at - a.created_at));
}

/** A result the task has since run past names both runs, so "Finished"
 * never stands beside a later run's different outcome. A Plan-mode run's
 * result is the plan it proposed. */
export function resultLabel(state: HostState, item: AttentionItem): string {
  const task = item.task_id ? state.tasks[item.task_id] : undefined;
  const plan = item.kind === "completed" && !!task && runMode(task, item.run_id ? state.runs[item.run_id] : undefined) === "plan";
  const outcome = item.kind === "completed" ? plan ? "Plan proposed" : "Finished" : "Failed";
  const latest = task && latestRun(task, state.runs);
  if (!task || !latest || !item.run_id || latest.run_id === item.run_id) return outcome;
  const runs = Object.values(state.runs).filter(r => r.task_id === task.task_id).sort((a, b) => a.admitted_at - b.admitted_at || a.run_id.localeCompare(b.run_id));
  const number = runs.findIndex(r => r.run_id === item.run_id) + 1;
  return `${number ? `Run ${number}` : "An earlier run"} ${plan ? "proposed a plan" : outcome.toLowerCase()} · run ${runs.length} ${RUN_END[latest.state].toLowerCase()}`;
}

/** The same open decisions counted by the Work badge, located by project. */
export function projectAttention(state: HostState) {
  return Object.values(state.projects).map(project => {
    const items = openAttention(state).filter(item => item.task_id && state.tasks[item.task_id]?.project_id === project.project_id);
    const reasons = [...new Set(items.map(item => item.kind === "approval" ? "Approval needed" : item.kind === "question" ? "Question waiting" : item.kind === "reconciliation_required" ? "Outcome unknown" : "Needs you"))];
    return { project, count: items.length, reason: reasons.join(" · ") };
  }).filter(group => group.count > 0);
}
