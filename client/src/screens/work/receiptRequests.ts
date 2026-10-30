// SPDX-License-Identifier: Apache-2.0
import type { ReceiptFact, TimelineItem } from "../../protocol/types";
import { applyDeliveries, withdrawnByStop } from "../../state/timeline";

export type ReceiptRequest = TimelineItem & { item: Extract<TimelineItem["item"], { kind: "user_message" }> };

/** The settlement records the exact prompt's sequence, including a queued
 * request written before its run existed. Never borrow a neighbouring bundle. */
export function receiptRequests(items: TimelineItem[], runId: string | undefined): ReceiptRequest[] {
  if (!runId) return [];
  const starts = new Map<number, string>();
  for (const row of items) if (row.run_id && row.item.kind === "delivery" && row.item.message_seq < row.seq)
    starts.set(row.item.message_seq, row.run_id);
  return applyDeliveries(items).filter((row): row is ReceiptRequest =>
    row.item.kind === "user_message" && (row.run_id ?? starts.get(row.seq)) === runId);
}

/** How many of the run's held messages its Stop withdrew. The computer's Stop
 * facts leave them out; its delivery records count them. */
export function heldWithdrawn(items: TimelineItem[], runId: string | undefined): number {
  const withdrawn = withdrawnByStop(items);
  return receiptRequests(items, runId).filter(r => withdrawn.has(r.seq)).length;
}

/** The Stop outcome that names those messages, beside the computer's own facts. */
export function heldWithdrawnFact(count: number): ReceiptFact {
  return { label: `Held messages withdrawn · ${count}`, status: "done", detail: null };
}
