// SPDX-License-Identifier: Apache-2.0
import type { AttentionItem, QueueView, Run, Task } from "../../protocol/types";
import { isActiveTask, type HostState } from "../../state/projection";
import { runMode } from "./logic";
import { unstartedQueueItem } from "./queueStatus";

export interface ReturnBrief {
  finished: string[];
  /** Finished Plan runs: a plan was proposed and nothing was carried out. */
  proposed: string[];
  failed: string[];
  changedFiles: number;
  checks: {
    passed: number;
    failed: number;
    unknown: number;
    notRun: number;
    reported: number;
    inferred: number;
  };
  pendingQuestions: number;
  remaining: number;
  queued: number;
  unknowns: string[];
}
/** Only recorded end times enter the interval. No status inferred from silence. */
export function buildBrief(
  projectId: string,
  since: number,
  tasks: Task[],
  runs: Run[],
  attention: AttentionItem[],
  through = Infinity,
  queue?: QueueView,
): ReturnBrief {
  const projectTasks = tasks.filter((t) => t.project_id === projectId);
  const ids = new Set(projectTasks.map((t) => t.task_id));
  const recent = runs
    .filter(
      (r) =>
        ids.has(r.task_id) &&
        r.ended_at !== null &&
        r.ended_at > since &&
        r.ended_at <= through,
    )
    .sort(
      (a, b) =>
        a.admitted_at - b.admitted_at || a.run_id.localeCompare(b.run_id),
    );
  const result: ReturnBrief = {
    finished: [],
    proposed: [],
    failed: [],
    changedFiles: 0,
    checks: { passed: 0, failed: 0, unknown: 0, notRun: 0, reported: 0, inferred: 0 },
    pendingQuestions: attention.filter(
      (a) =>
        a.task_id &&
        ids.has(a.task_id) &&
        a.resolved_at === null &&
        (a.kind === "question" || a.kind === "approval"),
    ).length,
    remaining: projectTasks.filter(task => {
      if (!isActiveTask(task)) return false;
      const run = runs.filter(r => r.task_id === task.task_id).sort((a, b) => b.admitted_at - a.admitted_at)[0];
      return run ? run.ended_at === null && ["dispatching", "working", "awaiting_approval", "awaiting_input", "stopping"].includes(run.state) : task.state !== "accepted";
    }).length,
    queued: projectTasks.filter(task => unstartedQueueItem(task, runs, queue)).length,
    unknowns: [],
  };
  const paths = new Set<string>();
  for (const run of recent) {
    const task = projectTasks.find(t => t.task_id === run.task_id);
    const title = task?.title ?? "this task";
    if (run.state === "completed") (task && runMode(task, run) === "plan" ? result.proposed : result.finished).push(run.task_id);
    if (run.state === "failed") result.failed.push(run.task_id);
    // A Stop ends a run before it reports, so that is not a missing update.
    if (!run.outcome)
      result.unknowns.push(run.state === "interrupted" ? `“${title}” was stopped before it reported a result.` : `The result of “${title}” hasn’t been reported.`);
    for (const file of run.outcome?.changed_files ?? []) paths.add(file.path);
    for (const check of run.outcome?.checks ?? []) {
      if (check.provenance === "native") result.checks.reported++;
      else if (check.provenance === "inferred") result.checks.inferred++;
      else if (check.status === "not_run") result.checks.notRun++;
      else result.checks[check.status]++;
    }
    result.unknowns.push(
      ...(run.outcome?.unknowns ?? []).map(s => `“${title}”: ${s}`),
      ...(run.outcome?.still_active ?? []).map((s) => `Still active from “${title}”: ${s}`),
    );
    for (const child of run.children)
      if (child.state === "active" || child.state === "unknown")
        result.unknowns.push(
          `${child.label} from “${title}” ${child.state === "active" ? "is still running" : "is unconfirmed"}.`,
        );
  }
  result.changedFiles = paths.size;
  result.finished = [...new Set(result.finished)];
  result.proposed = [...new Set(result.proposed)];
  result.failed = [...new Set(result.failed)];
  result.unknowns = [...new Set(result.unknowns)];
  return result;
}

/** Titles of tasks whose open reconciliation is for a run their own Stop
 * ended: what is left is checking that the Stop ended everything. */
export function stopChecks(state: HostState, attention: AttentionItem[]): string[] {
  const stopped = attention.filter((a) => a.kind === "reconciliation_required" && a.task_id && a.run_id
    && state.timelines[a.task_id]?.items.some((i) => i.run_id === a.run_id && i.item.kind === "receipt" && i.item.receipt.kind === "stop_task"));
  return [...new Set(stopped.map((a) => a.task_id!))].map((id) => state.tasks[id]?.title ?? "this task");
}

/** Sentences omit zero groups and keep computer checks separate from agent reports. */
export function digestSentences(brief: ReturnBrief, attentionCount?: number, stopped: string[] = []): { lead: string; details: string[] } {
  const count = (n: number, noun: string, verb: string) => `${n} ${noun}${n === 1 ? "" : "s"} ${verb}.`;
  const checks = stopped.map((title) => `Check that “${title}” stopped.`);
  const lead = attentionCount && checks.length === attentionCount ? checks[0]!
    : attentionCount ? count(attentionCount, "task update", attentionCount === 1 ? "needs attention" : "need attention")
    : brief.pendingQuestions && attentionCount === undefined ? count(brief.pendingQuestions, "request", brief.pendingQuestions === 1 ? "needs your decision" : "need your decision")
    : brief.failed.length ? count(brief.failed.length, "task", brief.failed.length === 1 ? "failed and needs review" : "failed and need review")
    : brief.unknowns.length ? brief.unknowns[0]!
    : brief.finished.length ? count(brief.finished.length, "task", "finished")
    : brief.proposed.length ? count(brief.proposed.length, "plan", brief.proposed.length === 1 ? "was proposed" : "were proposed")
    : brief.remaining ? count(brief.remaining, "task", brief.remaining === 1 ? "is still in progress" : "are still in progress")
    : brief.queued ? count(brief.queued, "task", brief.queued === 1 ? "is waiting in Next up" : "are waiting in Next up")
    : "No new task results to show.";
  const details = [
    ...checks.filter((check) => check !== lead),
    brief.unknowns.length && lead !== brief.unknowns[0] ? brief.unknowns[0]! : "",
    brief.finished.length && !lead.includes("finished") ? count(brief.finished.length, "task", "finished") : "",
    brief.proposed.length && !lead.includes("proposed") ? count(brief.proposed.length, "plan", brief.proposed.length === 1 ? "was proposed" : "were proposed") : "",
    brief.failed.length && !lead.includes("failed") ? count(brief.failed.length, "task", "failed") : "",
    brief.changedFiles ? count(brief.changedFiles, "file", brief.changedFiles === 1 ? "was changed" : "were changed") : "",
    brief.checks.passed ? `${brief.checks.passed} ${brief.checks.passed === 1 ? "check" : "checks"} passed on the computer.` : "",
    brief.checks.failed ? count(brief.checks.failed, "computer check", "failed") : "",
    brief.checks.reported ? `${brief.checks.reported} ${brief.checks.reported === 1 ? "check was" : "checks were"} only reported by the agent.` : "",
    brief.checks.inferred ? `${brief.checks.inferred} ${brief.checks.inferred === 1 ? "check was inferred and remains" : "checks were inferred and remain"} unconfirmed.` : "",
    brief.checks.unknown ? count(brief.checks.unknown, "computer check", brief.checks.unknown === 1 ? "has an unknown result" : "have unknown results") : "",
    brief.checks.notRun ? `${brief.checks.notRun} ${brief.checks.notRun === 1 ? "check was" : "checks were"} not run.` : "",
    brief.remaining && !lead.includes("progress") ? count(brief.remaining, "task", brief.remaining === 1 ? "is still in progress" : "are still in progress") : "",
    brief.queued && !lead.includes("Next up") ? count(brief.queued, "task", brief.queued === 1 ? "is waiting in Next up" : "are waiting in Next up") : "",
  ].filter(Boolean);
  return { lead, details };
}
