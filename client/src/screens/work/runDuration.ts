// SPDX-License-Identifier: Apache-2.0
import type { Run, TimelineItem } from "../../protocol/types";
import { duration, shortAge } from "../../design/time";

/** Reconciliation can update ended_at hours later; only agent evidence measures work. */
export function runDuration(run: Run, items: TimelineItem[], now = Date.now()): string {
  const evidence = items.filter(i => i.run_id === run.run_id);
  const end = evidence.find(i => i.item.kind === "run_ended" && i.item.run_id === run.run_id);
  if (end?.item.kind === "run_ended" && ["completed", "failed", "interrupted"].includes(end.item.state))
    return duration(Math.max(0, end.at - run.admitted_at));
  if (run.ended_at === null && ["admitted", "dispatching", "working", "awaiting_approval", "awaiting_input", "stopping"].includes(run.state)) {
    const elapsed = Math.max(0, now - run.admitted_at);
    return `Running · ${elapsed < 60_000 ? duration(elapsed) : shortAge(elapsed)} so far`;
  }
  const times = evidence.filter(i => ["agent_message", "activity", "approval", "run_ended"].includes(i.item.kind)).map(i => i.at);
  if (run.acknowledged_at !== null) times.push(run.acknowledged_at);
  if (!times.length) return "Duration not reported";
  return `At least ${duration(Math.max(0, Math.max(...times) - run.admitted_at))}`;
}
