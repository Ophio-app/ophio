// SPDX-License-Identifier: Apache-2.0
//
// What a run came to, for the top of its receipt: what it needs from the
// person, what it left undone, the separate work it found, or that it is
// done. The agent's own report says which where it gave one (see
// `ophio-outcome` in the agents' instructions); otherwise the run's state
// does. The agent's word decides what is offered next, never what is
// verified: a turn it called done that left a failed check, a helper
// running or an unknown is finished, and needs a look, not done.

import { readOnlyLink } from "../../app/github";
import type { CheckResult, NewWork, Run, Shown, Task, TimelineItem } from "../../protocol/types";
import { runDoubts } from "./logic";

export type OutcomeKind = "waiting" | "needs_you" | "watching" | "unfinished" | "stopped" | "failed" | "done";

export interface Outcome {
  kind: OutcomeKind;
  title: string;
  /** A line under the title; null when the title and items say it all. */
  detail: string | null;
  /** The agent's questions, or what it left undone. */
  items: string[];
  /** Separate work it proposes, each to start as a task of its own. */
  newWork: NewWork[];
  /** The agent said so, rather than Ophio reading it from the run. */
  reported: boolean;
  /** What it left in doubt, where it is otherwise done: a done outcome with
   * any is titled "Finished, needs a look". */
  doubts: string[];
}

export function runOutcome(task: Task, run: Run | undefined): Outcome | null {
  if (!run) return null;
  const report = run.outcome?.report;
  // A review files what it finds as issues itself; those are its new work.
  const newWork = readOnlyLink(task.github?.link) ? [] : (report?.new_work ?? []);
  const said = { newWork, reported: !!report, doubts: [] as string[] };
  const left = report?.remaining ?? [];
  switch (run.state) {
    case "awaiting_approval":
      return { ...said, kind: "waiting", title: "Waiting for you", detail: "It’s asking for your approval in the conversation.", items: [] };
    case "awaiting_input":
      return { ...said, kind: "waiting", title: "Waiting for you", detail: "It asked you something in the conversation.", items: [] };
    case "admitted": case "dispatching": case "working": case "stopping": case "reconciliation_required":
      return null;
  }
  if (run.human_resolution?.resolution === "mark_failed")
    return { ...said, kind: "failed", title: "Didn’t finish", detail: "You marked it failed. Say what to fix, or have it try again.", items: left };
  if (run.state === "failed")
    return { ...said, kind: "failed", title: "Didn’t finish", detail: "The run failed before it was done.", items: [...left, ...(run.outcome?.unknowns ?? [])] };
  if (run.state === "interrupted")
    return { ...said, kind: "stopped", title: "Stopped before it finished", detail: left.length ? "Left to do:" : "Carry on to pick up where it stopped.", items: left };
  if (report?.status === "needs_you")
    return { ...said, kind: "needs_you", title: "It needs you", detail: report.questions.length ? null : "It needs your answer or action to go on.", items: report.questions };
  // It asked Ophio to wake it to go on, or asked another agent and waits
  // for the answer, so nothing is the person's to do; that is what is left.
  if (task.wake && run.run_id === task.current_run)
    return { ...said, kind: "watching", title: task.wake.answer ? "Waiting for an answer" : "Watching", detail: task.wake.reason, items: [] };
  if (report?.status === "unfinished")
    return { ...said, kind: "unfinished", title: "Not finished", detail: left.length ? "Left to do:" : "It stopped with work left.", items: left };
  const doubts = runDoubts(run);
  if (doubts.length) {
    const checks = run.outcome?.checks ?? [];
    // Each failed check no later run of the same check replaced; a denied
    // one never ran, so it replaces nothing.
    const key = (c: CheckResult) => c.command ?? c.name;
    const failed = run.outcome?.verification
      ? checks.flatMap((c, i) => c.status === "failed"
        && !checks.slice(i + 1).some((later) => later.status !== "not_run" && key(later) === key(c))
        ? [`${key(c)} failed`] : [])
      : checks.filter((c) => c.status === "failed").map((c) => `${c.name} failed`);
    return { ...said, doubts, kind: "done", title: "Finished, needs a look", detail: `${doubts.join(" · ")}.`, items: failed };
  }
  return { ...said, kind: "done", title: "Done", detail: null, items: [] };
}

/** What Carry on and Try again send as the task's next turn. */
export const CARRY_ON = "Carry on with what’s left.";
export const TRY_AGAIN = "That didn’t work. Try again, and say what you changed.";

const MODE: Record<NewWork["mode"], string> = { work: "Work", plan: "Plan", ask: "Ask" };

/** One line about a piece of new work: how it would run, and its issue. */
export function newWorkMeta(w: NewWork): string {
  return [MODE[w.mode], w.issue ? `#${w.issue}` : ""].filter(Boolean).join(" · ");
}

/** Something a run showed the person, where the conversation has it. */
export interface RunShown {
  key: string;
  at: number;
  shown: Shown;
}

/** What a run showed the person, oldest first: each picture, preview or ask
 * once, where it was last shown. */
export function runShown(items: TimelineItem[], runId: string): RunShown[] {
  const latest = new Map<string, RunShown>();
  for (const it of items) {
    if (it.run_id !== runId || it.item.kind !== "shown") continue;
    const s = it.item.shown;
    const id = s.what === "media" ? `m${s.artifact_id}` : s.what === "preview" ? `p${s.preview_id}`
      : s.what === "checklist" ? `c${s.title}` : `a${s.action}`;
    latest.delete(id);
    latest.set(id, { key: `s${it.seq}`, at: it.at, shown: s });
  }
  return [...latest.values()];
}

/** Whether `items` reach back to where the run started, so they hold all it showed. */
export function reachesRunStart(items: TimelineItem[], runId: string): boolean {
  return items.some((it) => it.item.kind === "run_started" && it.item.run_id === runId);
}
