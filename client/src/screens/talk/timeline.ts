// SPDX-License-Identifier: Apache-2.0
//
// A task's journal timeline shaped for reading: the person's words and the
// agent's replies stay prominent, consecutive tool steps fold into one quiet
// group, and run boundaries become dividers. Delivery updates belong to
// their message; every activity is reachable by expanding its group.

import { applyDeliveries, withdrawnByRevocation, withdrawnByStop } from "../../state/timeline";
import type { AnnotationBundle, DeliveryState, NotRun, ReceiptFact, Run, HumanCheck, HumanResolution, RunState, Shown, TaskMode, TimelineItem } from "../../protocol/types";

export type ActivityItem = TimelineItem & { item: Extract<TimelineItem["item"], { kind: "activity" }> };
/** A tool call as one row: its start, updated in place by later reports
 * with the same key; `output` is the finished call's detail. */
export type Step = ActivityItem & { output?: string | null };

export type Row =
  /** `withdrawn`: Stop withdrew the message while it was held, so it was never sent.
   * `carried`: the earlier plan the computer recorded this message as carrying out. */
  | { kind: "user"; messageSeq: number; key: string; at: number; text: string; bundle: AnnotationBundle | null; delivery: DeliveryState; withdrawn: boolean; revoked: boolean; carried: string | null }
  | { kind: "agent"; key: string; at: number; text: string; partial: boolean }
  /** `ended`: the run finished, so a step still marked running will not report a result. */
  | { kind: "steps"; key: string; at: number; steps: Step[]; ended: boolean }
  | { kind: "approval"; key: string; at: number; approvalId: string; runId: string | null }
  | { kind: "run_started"; key: string; at: number; runId: string; number: number }
  /** `withdrawn`: how many held messages Stop withdrew from this run. */
  /** `mode`: as the run recorded it, absent on runs recorded before it was kept. */
  | { kind: "run_ended"; key: string; at: number; runId: string; state: RunState; mode: TaskMode | undefined; withdrawn: number }
  | { kind: "receipt"; key: string; at: number; summary: string; outcome: ReceiptOutcome }
  | { kind: "decision"; key: string; at: number; runId: string; decision: HumanResolution }
  | { kind: "helpers_checked"; key: string; at: number; runId: string; decision: HumanCheck }
  | { kind: "note"; key: string; at: number; text: string }
  /** What the agent showed the person, or asked them to do, through Ophio. */
  | { kind: "shown"; key: string; at: number; shown: Shown };

/** A receipt is confirmed only when every fact is; any failure outranks an open one. */
export type ReceiptOutcome = "confirmed" | "open" | "failed";

function outcome(facts: ReceiptFact[]): ReceiptOutcome {
  if (facts.some((f) => f.status === "failed")) return "failed";
  const settled = facts.every((f) => f.status === "done" || f.status === "not_applicable");
  return facts.length > 0 && settled ? "confirmed" : "open";
}

/** Keep late receipts with their run even when a newer run has already begun. */
export function conversationRuns(items: TimelineItem[], runs: Run[]): { items: TimelineItem[]; ids: string[] } {
  const starts = new Map<string, number>();
  for (const item of items) if (item.run_id && !starts.has(item.run_id)) starts.set(item.run_id, item.at);
  for (const run of runs) starts.set(run.run_id, run.admitted_at);
  const ids = [...starts.keys()].sort((a, b) => (starts.get(a) ?? 0) - (starts.get(b) ?? 0));
  const groups = new Map<string | null, TimelineItem[]>();
  let context: string | null = null;
  const receipts = new Set<string>();
  for (const item of items) {
    const receipt = item.item.kind === "receipt" ? item.item.receipt : null;
    // Old control events sometimes lack a run link. Their request time is the
    // historical context, not the run selected when a cache page arrives.
    const historical = receipt && !item.run_id ? [...runs].filter(r => r.admitted_at <= receipt.requested_at).sort((a, b) => b.admitted_at - a.admitted_at)[0]?.run_id : null;
    const runId: string | null = item.run_id ?? historical ?? context;
    if (!receipt) context = runId;
    if (receipt && ["take_over", "leave_control", "pause_input"].includes(receipt.kind)) {
      const identity = `${runId}/${JSON.stringify(receipt)}`;
      if (receipts.has(identity)) continue;
      receipts.add(identity);
    }
    const group = groups.get(runId) ?? [];
    group.push(item.run_id === runId ? item : { ...item, run_id: runId });
    groups.set(runId, group);
  }
  return { items: [...(groups.get(null) ?? []), ...ids.flatMap(id => groups.get(id) ?? [])], ids };
}

export function rows(items: TimelineItem[], runs: Run[] = []): Row[] {
  const out: Row[] = [];
  const calls = new Map<string, Step>();
  const grouped = conversationRuns(applyDeliveries(items), [...runs]);
  const withdrawn = withdrawnByStop(items),
    revoked = withdrawnByRevocation(items);
  const withdrawnFrom = new Map<string, number>();
  const started = new Set<string>();
  let currentRun: string | null = null;
  for (const it of grouped.items) {
    const key = `s${it.seq}`;
    const b = it.item;
    if (it.run_id && it.run_id !== currentRun && b.kind !== "delivery") {
      currentRun = it.run_id;
      calls.clear();
      const number = grouped.ids.indexOf(currentRun) + 1;
      if (number > 1 && b.kind !== "run_started") {
        const run = runs.find(r => r.run_id === currentRun);
        out.push({ kind: "run_started", key: `run-${currentRun}`, at: run?.admitted_at ?? it.at, runId: currentRun, number });
        started.add(currentRun);
      }
    }
    switch (b.kind) {
      case "delivery":
        break;
      case "activity": {
        const call = b.key ? calls.get(b.key) : undefined;
        if (call) {
          // A progress report keeps the start's wording; a result replaces it.
          if (b.status !== "running") {
            call.item = { ...call.item, summary: b.summary, status: b.status, not_run: b.not_run };
            call.output = b.detail;
          }
          break;
        }
        const step: Step = { ...(it as ActivityItem), item: { ...b } };
        if (b.key) calls.set(b.key, step);
        const last = out.at(-1);
        if (last?.kind === "steps") last.steps.push(step);
        else out.push({ kind: "steps", key, at: it.at, steps: [step], ended: false });
        break;
      }
      case "user_message":
        out.push({ kind: "user", messageSeq: it.seq, key, at: it.at, text: b.text, bundle: b.bundle, delivery: b.delivery, withdrawn: withdrawn.has(it.seq), revoked: revoked.has(it.seq), carried: b.carried_plan?.text ?? null });
        if (it.run_id && withdrawn.has(it.seq)) withdrawnFrom.set(it.run_id, (withdrawnFrom.get(it.run_id) ?? 0) + 1);
        break;
      case "agent_message":
        out.push({ kind: "agent", key, at: it.at, text: b.text, partial: b.partial });
        break;
      case "approval":
        out.push({ kind: "approval", key, at: it.at, approvalId: b.approval_id, runId: it.run_id });
        break;
      case "run_started":
        if (!started.has(b.run_id)) out.push({ kind: "run_started", key, at: it.at, runId: b.run_id, number: grouped.ids.indexOf(b.run_id) + 1 });
        started.add(b.run_id);
        break;
      case "run_ended":
        for (const r of out) if (r.kind === "steps" && r.steps[0]?.run_id === b.run_id) r.ended = true;
        out.push({ kind: "run_ended", key, at: it.at, runId: b.run_id, state: b.state, mode: runs.find(r => r.run_id === b.run_id)?.mode, withdrawn: withdrawnFrom.get(b.run_id) ?? 0 });
        break;
      case "receipt":
        out.push({ kind: "receipt", key, at: it.at, summary: b.receipt.summary, outcome: outcome(b.receipt.facts) });
        break;
      case "note":
        out.push({ kind: "note", key, at: it.at, text: b.text });
        break;
      case "shown":
        out.push({ kind: "shown", key, at: it.at, shown: b.shown });
        break;
    }
  }
  // Run admission can arrive before its first timeline entry.
  for (const run of runs) {
    const number = grouped.ids.indexOf(run.run_id) + 1;
    if (number > 1 && !started.has(run.run_id) && !items.some(i => i.run_id === run.run_id))
      out.push({ kind: "run_started", key: `run-${run.run_id}`, at: run.admitted_at, runId: run.run_id, number });
  }
  for (const row of out) {
    if (row.kind === "steps" && runs.some(run => run.run_id === row.steps[0]?.run_id && ["interrupted", "completed", "failed", "reconciliation_required"].includes(run.state))) row.ended = true;
  }
  for (const run of runs) {
    if (run.helpers_checked) {
      const last = out.findLastIndex(row => ("runId" in row && row.runId === run.run_id) || grouped.items.some(i => i.run_id === run.run_id && `s${i.seq}` === row.key));
      out.splice(last < 0 ? out.length : last + 1, 0, { kind: "helpers_checked", key: `helpers-${run.run_id}`, at: run.helpers_checked.at, runId: run.run_id, decision: run.helpers_checked });
    }
    if (!run.human_resolution) continue;
    const last = out.findLastIndex(row => ("runId" in row && row.runId === run.run_id) || grouped.items.some(i => i.run_id === run.run_id && `s${i.seq}` === row.key));
    out.splice(last < 0 ? out.length : last + 1, 0, { kind: "decision", key: `decision-${run.run_id}`, at: run.human_resolution.at, runId: run.run_id, decision: run.human_resolution });
  }
  return out;
}

/** `next`, with each row that repeats a row of `prev` unchanged replaced by
 * that earlier row, so memoized views of unchanged rows need not render again. */
export function keepUnchanged(prev: Row[], next: Row[]): Row[] {
  const earlier = new Map(prev.map((row) => [row.key, row]));
  return next.map((row) => {
    const before = earlier.get(row.key);
    return before && sameData(before, row) ? before : row;
  });
}

function sameData(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null || Array.isArray(a) !== Array.isArray(b)) return false;
  const keys = Object.keys(a);
  return keys.length === Object.keys(b).length && keys.every((k) => Object.hasOwn(b, k) && sameData((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]));
}

/** A step that never ran, by why: its label, and its words after a count. */
export const NOT_RUN: Record<NotRun, { label: string; counted: string }> = {
  denied: { label: "Denied by you", counted: "denied by you" },
  stopped: { label: "Not run (stopped)", counted: "not run (stopped)" },
};

/** "4 steps · 1 failed", led by the most recent step's summary. A step that
 * never ran is counted by why, not as failed. */
export function stepsSummary(steps: ActivityItem[], ended = false): { latest: string; count: string; failed: number; notRun: Record<NotRun, number>; running: boolean } {
  const failed = steps.filter((s) => s.item.status === "failed" && !s.item.not_run).length;
  const notRun = { denied: 0, stopped: 0 };
  for (const s of steps) if (s.item.not_run) notRun[s.item.not_run]++;
  const running = !ended && steps.some((s) => s.item.status === "running");
  const latest = stepSummary(steps.at(-1)!, ended);
  const n = steps.length;
  return { latest, count: n === 1 ? "1 step" : `${n} steps`, failed, notRun, running };
}

export const RUN_END: Record<RunState, string> = {
  admitted: "Waiting to start",
  dispatching: "Starting",
  working: "Working",
  awaiting_approval: "Waiting for approval",
  awaiting_input: "Waiting for an answer",
  stopping: "Stopping",
  interrupted: "Stopped",
  completed: "Finished",
  failed: "Failed",
  reconciliation_required: "Outcome unknown",
};

/** How a run ended; a finished Plan run proposed a plan and changed nothing. */
export function runEnd(state: RunState, mode: TaskMode | null | undefined, conversation = false): string {
  if (state === "completed" && conversation) return "Replied";
  return state === "completed" && mode === "plan" ? "Plan proposed" : RUN_END[state];
}

/** Short description of an attached bundle, e.g. "2 images · 3 marks". */
export function bundleSummary(b: AnnotationBundle): string {
  const n = (k: number, one: string, many: string) => (k ? `${k} ${k === 1 ? one : many}` : "");
  return (
    [
      n(b.sources.length, "image", "images"),
      n(b.marks.filter((m) => !m.deleted).length, "mark", "marks"),
      n(b.comments.length, "comment", "comments"),
      n(b.steps.length, "step", "steps"),
      b.speech ? "voice" : "",
    ]
      .filter(Boolean)
      .join(" · ") || "Markup"
  );
}

/** Older journals embedded live state in the title. Keep their tool name readable. */
export function stepSummary(step: ActivityItem, ended: boolean): string {
  if (step.item.tool === "ExitPlanMode") return "Plan proposed";
  if (step.item.not_run) return `${step.item.tool}: ${NOT_RUN[step.item.not_run].label}`;
  return ended || step.item.status !== "running"
    ? step.item.summary.replace(/^Running /i, "").replace(/ is running$/i, "")
    : step.item.summary;
}
