// SPDX-License-Identifier: Apache-2.0
//
// How far a stopped run had got, from its own timeline before Stop: the
// agent's last words and its last step. The computer's Stop receipt says only
// what Stop did, so without this the receipt is silent on the work itself.

import type { InterventionReceipt, ReceiptFact, TimelineItem } from "../../protocol/types";
import { NOT_RUN, rows, stepSummary } from "../talk/timeline";
import { toolInput } from "../talk/toolInput";

export interface StopProgress {
  /** The agent's last message before Stop, shortened to a line or two. */
  said: string | null;
  finished: number;
  failed: number;
  /** `input`: the path, command or task the step names, when it names one. */
  last: { name: string; input: string | null; ended: string } | null;
}

/** `text` on one line, cut at a word boundary to about `limit` characters. */
export function excerpt(text: string, limit = 140): string {
  const flat = text.replace(/\s+/g, " ").trim();
  if (flat.length <= limit) return flat;
  const cut = flat.slice(0, limit);
  const space = cut.lastIndexOf(" ");
  return `${(space > limit / 2 ? cut.slice(0, space) : cut).replace(/[\s,;:.—–-]+$/, "")}…`;
}

/** A run's Stop receipt, scoped to the run by the companion's timeline entry. */
export function stopReceipt(items: TimelineItem[], runId: string): InterventionReceipt | undefined {
  const entry = items.filter((i) => i.run_id === runId && i.item.kind === "receipt" && i.item.receipt.kind === "stop_task").at(-1)?.item;
  return entry?.kind === "receipt" ? entry.receipt : undefined;
}

/** What Stop could not confirm stays open after the run, wherever the run is summed up. */
export function stopUnknowns(receipt: InterventionReceipt | undefined): ReceiptFact[] {
  return receipt?.facts.filter((f) => f.status !== "done" && f.status !== "not_applicable") ?? [];
}

export function stopFactWord(fact: ReceiptFact): string {
  switch (fact.status) {
    case "done": return fact.label.startsWith("Child:") ? "Ended" : "Confirmed";
    case "still_active": return "Still running";
    case "unknown": return "Unknown";
    case "pending": return "Awaiting confirmation";
    case "failed": return "Could not stop";
    case "not_applicable": return "Not applicable";
  }
}

export function stopProgress(items: TimelineItem[], runId: string): StopProgress | null {
  const run = items.filter((i) => i.run_id === runId);
  const stop = run.findLast((i) => i.item.kind === "receipt" && i.item.receipt.kind === "stop_task");
  const before = stop ? run.filter((i) => i.seq < stop.seq) : run;
  const message = before.findLast((i) => i.item.kind === "agent_message" && i.item.text.trim());
  const said = message?.item.kind === "agent_message" ? excerpt(message.item.text) : null;
  const steps = rows(before).flatMap((row) => (row.kind === "steps" ? row.steps : []));
  const step = steps.at(-1);
  if (!said && !step) return null;
  const finished = steps.filter((s) => s.item.status === "succeeded" && !s.item.not_run).length;
  const failed = steps.filter((s) => s.item.status === "failed" && !s.item.not_run).length;
  let last: StopProgress["last"] = null;
  if (step) {
    const { tool, detail, status, not_run } = step.item;
    const input = detail ? toolInput(tool, detail) : null;
    last = {
      // A step that names its input reads as the tool and that input; one that
      // names none keeps the summary the transcript shows for it.
      name: input ? tool : stepSummary(step, true),
      input: input ? excerpt(input.text.split("\n")[0] ?? "", 80) || null : null,
      ended: not_run
        ? NOT_RUN[not_run].counted
        : status === "succeeded"
          ? "finished"
          : status === "failed"
            ? "failed"
            : "did not finish",
    };
  }
  return { said, finished, failed, last };
}
