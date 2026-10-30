// SPDX-License-Identifier: Apache-2.0

import { CircleAlert, CircleCheck, FileWarning, MessageCircleQuestion, ShieldAlert, Unplug, UserRound, type LucideIcon } from "lucide-react";
import type { AttentionItem, AttentionKind } from "../protocol/types";

export const ATTENTION: Record<AttentionKind, { label: string; icon: LucideIcon; group: "decide" | "problem" | "review" }> = {
  approval: { label: "Approval waiting", icon: ShieldAlert, group: "decide" },
  question: { label: "Question waiting", icon: MessageCircleQuestion, group: "decide" },
  waiting_for_you: { label: "Waiting for you", icon: UserRound, group: "decide" },
  failure: { label: "Task failed", icon: CircleAlert, group: "review" },
  reconciliation_required: { label: "Needs a check", icon: CircleAlert, group: "problem" },
  conflict: { label: "File conflict", icon: FileWarning, group: "problem" },
  connection_issue: { label: "Connection issue", icon: Unplug, group: "problem" },
  completed: { label: "Ready to review", icon: CircleCheck, group: "review" },
};

export const GROUP_TITLE = { decide: "Needs a decision", problem: "Problems", review: "Ready to review" } as const;

export function attentionTone(kind: AttentionKind): "warn" | "stop" | "ok" {
  const g = ATTENTION[kind].group;
  return g === "decide" ? "warn" : g === "problem" ? "stop" : "ok";
}

/** Short label for the entry: the most urgent kind, plus how many more. */
export function entryLabel(items: AttentionItem[]): string {
  const top = items[0];
  if (!top) return "";
  const same = items.filter((i) => i.kind === top.kind).length;
  if (same > 1 && top.kind === "approval") return `${same} approvals waiting`;
  if (same > 1 && top.kind === "completed") return `${same} ready to review`;
  return ATTENTION[top.kind].label;
}

/**
 * What the item is about, in the person's terms: its own title unless that
 * only repeats the kind ("Approval waiting"), then its detail or task.
 */
export function attentionWhat(item: AttentionItem, taskTitle?: string): string {
  if (taskTitle) return taskTitle;
  const generic = item.title.trim().toLowerCase() === ATTENTION[item.kind].label.toLowerCase();
  if (!generic && item.title.trim()) return item.title;
  return item.body || taskTitle || ATTENTION[item.kind].label;
}
