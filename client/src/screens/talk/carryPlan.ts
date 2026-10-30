// SPDX-License-Identifier: Apache-2.0
import { useEffect } from "react";
import { shallowEqual, useApp, useHost, useLink, type App } from "../../app/context";
import { readOnlyLink } from "../../app/github";
import type { Run, Task, TimelineItem } from "../../protocol/types";
import { taskRuns, type HostState, type TimelineState } from "../../state/projection";
import { sideRun } from "../work/logic";
import { sendOnOpen } from "./sendOnOpen";
import { rows } from "./timeline";
import { toolInput } from "./toolInput";

/** A finished Plan that can be carried out. What a find, sort or check
 * reports is acted on by the issues and fixes it leads to, never carried out. */
export const finishedPlan = (task: Task, run?: Run | null) => task.mode === "plan" && task.state === "completed" && (!run || run.state === "completed") && !readOnlyLink(task.github?.link);
export interface CarryPlan { text: string; runId: string; number: number; latest: boolean }

/** The order that gives a run its number within its task. */
export const byAdmission = (a: Run, b: Run): number => a.admitted_at - b.admitted_at || a.run_id.localeCompare(b.run_id);

/** Offering and loading share this lookup. Never choose from an incomplete page. */
export function findCarryPlan(task: Task, state: Pick<HostState, "runs" | "timelines">): CarryPlan | null {
  // No run makes a task that is not a finished plan into one.
  if (!finishedPlan(task)) return null;
  const runs = taskRuns(state, task.task_id).sort(byAdmission);
  // A question asked since, such as Summarize, neither proposes nor replaces the plan.
  const own = runs.filter(run => !sideRun(task, run));
  if (!finishedPlan(task, own.at(-1))) return null;
  const timeline = state.timelines[task.task_id];
  if (!timeline?.loaded || timeline.hasMore) return null;
  for (let index = runs.length - 1; index >= 0; index--) {
    const run = runs[index]!;
    if (sideRun(task, run)) continue;
    const text = runPlan(run, timeline.items);
    if (text) return { text, runId: run.run_id, number: index + 1, latest: run === own.at(-1) };
  }
  return null;
}

/** What a run proposed: its last plan, else its replies, else its outcome. */
export function runPlan(run: Run, timeline: TimelineItem[]): string | undefined {
  const items = timeline.filter(i => i.run_id === run.run_id);
  const proposal = rows(items, [run]).flatMap(row => row.kind === "steps" ? row.steps : [])
    .filter(step => step.item.tool === "ExitPlanMode")
    .map(step => step.item.detail ? toolInput("ExitPlanMode", step.item.detail)?.text : undefined).findLast(text => text?.trim());
  const messages = items.filter(i => i.item.kind === "agent_message" && !i.item.partial)
    .map(i => i.item.kind === "agent_message" ? i.item.text : "").join("\n\n").trim();
  return proposal?.trim() || messages || run.outcome?.summary?.trim() || undefined;
}

const loading = new WeakMap<App["session"], Map<string, Promise<void>>>();
async function loadPlanHistory(session: App["session"], taskId: string): Promise<void> {
  let tasks = loading.get(session);
  if (!tasks) { tasks = new Map(); loading.set(session, tasks); }
  const pending = tasks.get(taskId);
  if (pending) return pending;
  const request = (async () => {
    await session.loadTimeline(taskId);
    while (session.state.get().timelines[taskId]?.hasMore) {
      const count = session.state.get().timelines[taskId]!.items.length;
      await session.loadTimeline(taskId, true);
      if (session.state.get().timelines[taskId]!.items.length === count) break;
    }
  })();
  tasks.set(taskId, request);
  try { await request; } finally { tasks.delete(taskId); }
}

export function useCarryPlan(task: Task | null | undefined): CarryPlan | null {
  const { session } = useApp();
  const online = useLink().state === "online";
  const eligible = !!task && finishedPlan(task);
  const taskId = task?.task_id;
  useEffect(() => eligible && taskId ? session.watchTimeline(taskId) : undefined, [session, eligible, taskId]);
  useEffect(() => {
    if (eligible && taskId && online) void loadPlanHistory(session, taskId).catch(() => undefined);
  }, [session, eligible, taskId, task?.revision, online]);
  return useHost(s => task ? findCarryPlan(task, s) : null, shallowEqual);
}

/** Opening the review only prepares a draft. Work is authorized by its Start
 * action, which `send` takes at once unless Review needs a choice made or the
 * message holds the person's own words, which they see before they go. */
export async function reviewPlan(app: App, task: Task, stillCurrent: () => boolean = () => true, opts: { send?: boolean } = {}): Promise<void> {
  const { session, ui } = app;
  if (!finishedPlan(task)) throw new Error("Finish the plan before carrying it out.");
  const release = session.watchTimeline(task.task_id);
  try {
    await loadPlanHistory(session, task.task_id);
    if (!stillCurrent()) return;
    const state = session.state.get();
    const current = state.tasks[task.task_id];
    if (!current || !finishedPlan(current) || current.current_run !== task.current_run || current.revision !== task.revision) throw new Error("The plan changed. Open its latest result and review it again.");
    const plan = findCarryPlan(current, state);
    if (!plan) throw new Error("The agent hasn't supplied plan text to carry out. Ask it to finish the plan first.");
    const draft = ui.draft(task.project_id, task.task_id);
    if (draft?.attempt) throw new Error("Check the previous send before preparing this plan.");
    // The draft keeps only the person's words; the plan rides on the route, quoted.
    const words = draft?.text.trim();
    ui.saveDraft(task.project_id, task.task_id, { text: words || "Carry out this plan and report the changes and checks.", mode: "work" });
    if (opts.send && !words) sendOnOpen(task.project_id, task.task_id);
    ui.push({ screen: "review", projectId: task.project_id, taskId: task.task_id, intent: "carry_out_plan", plan });
  } finally {
    release();
  }
}

const PLAN_END = "--- End of plan ---";
const planStart = (number: number) => `--- Plan from run ${number}, proposed by the agent ---`;

/** What Start Work sends: the person's message, then the agent's plan between markers. */
export const withPlan = (message: string, plan: Pick<CarryPlan, "text" | "number">): string =>
  `${message}\n\n${planStart(plan.number)}\n${plan.text}\n${PLAN_END}`;

/** Separates a sent carry-out message so the plan never reads as the person's
 * words. `plan` must be known to be carried; markers alone prove nothing. */
export function carriedPlan(text: string, plan: string | null | undefined): { message: string; number: number; plan: string } | null {
  const tail = `\n${plan}\n${PLAN_END}`;
  if (!plan || !text.endsWith(tail)) return null;
  const head = text.slice(0, -tail.length);
  const start = /\n\n--- Plan from run (\d+), proposed by the agent ---$/.exec(head);
  return start ? { message: head.slice(0, start.index), number: Number(start[1]), plan } : null;
}

/** The plan a sent message carried: the computer's record, else, for a message
 * sent before it recorded carries, a quote checked against the whole history. */
export function sentPlan(text: string, recorded: string | null | undefined, runs: Run[], timeline: Pick<TimelineState, "items" | "hasMore">): string | undefined {
  return recorded ?? (timeline.hasMore ? undefined : earlierCarry(text, n => {
    const run = runs.toSorted(byAdmission)[n - 1];
    return run && runPlan(run, timeline.items);
  }));
}

/** For a message sent before the computer recorded carries: the quoted text
 * is run N's plan only if it is exactly what run N proposed. */
export function earlierCarry(text: string, planOf: (number: number) => string | undefined): string | undefined {
  const at = text.lastIndexOf("\n\n--- Plan from run ");
  const quoted = at < 0 ? null : /^\n\n--- Plan from run (\d+), proposed by the agent ---\n([\s\S]*)\n--- End of plan ---$/.exec(text.slice(at));
  return quoted && planOf(Number(quoted[1])) === quoted[2] ? quoted[2] : undefined;
}
