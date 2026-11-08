// SPDX-License-Identifier: Apache-2.0
//
// What a notification is about, in the words the person chooses by, and how
// each shows: Alert, Silent or Off. The list, its words and the recommended
// choices are the computer's own (`protocol::notifications::AlertType`),
// exported to alertTypes.json, so Settings, Android's categories and the
// watcher never word a type differently.

import list from "../protocol/generated/alertTypes.json";
import type { AlertTopic, AlertType, AttentionItem, NotificationSettings, ProjectNotifications, Tier, TypeTier } from "../protocol/types";

/** The computer's word that it labels each item with its type, keeps each
 * project's choice per type, takes "handled" and the task bell. */
export const ALERT_TYPES_FEATURE = "alert_types_v1";

export type TypeGroup = "needs_you" | "worth_a_look" | "in_the_app";
export interface AlertTypeInfo {
  type: AlertType;
  name: string;
  description: string;
  group: TypeGroup;
  /** The recommended choice. */
  tier: Tier;
}

/** Every type, in the order Settings lists them. */
export const ALERT_TYPES = list as readonly AlertTypeInfo[];
const byType = new Map(ALERT_TYPES.map((t) => [t.type, t]));
export function typeInfo(type: AlertType): AlertTypeInfo {
  return byType.get(type) ?? { type, name: type, description: "", group: "worth_a_look", tier: "alert" };
}
export const isAlertType = (value: unknown): value is AlertType => typeof value === "string" && byType.has(value as AlertType);
export const recommendedTier = (type: AlertType): Tier => typeInfo(type).tier;
export function recommendedTiers(): Record<AlertType, Tier> {
  return Object.fromEntries(ALERT_TYPES.map((t) => [t.type, t.tier])) as Record<AlertType, Tier>;
}
export const isTier = (value: unknown): value is Tier => value === "alert" || value === "silent" || value === "off";

/** Settings' three headings, in order. */
export const GROUPS: readonly { group: TypeGroup; title: string; hint: string }[] = [
  { group: "needs_you", title: "Needs you", hint: "Something waits on you and won’t move until you act." },
  { group: "worth_a_look", title: "Worth a look", hint: "Good to know, but nothing is stuck." },
  { group: "in_the_app", title: "Only in the app", hint: "Routine progress. It waits in Øphio until you look." },
];

export const TIER_LABELS: Record<Tier, string> = { alert: "Alert", silent: "Silent", off: "Off" };
/** What each choice does, said once under the list. */
export const TIER_HINTS: Record<Tier, string> = {
  alert: "Sound, vibration and a banner.",
  silent: "In the notification list, with no sound or banner.",
  off: "No notification. It still waits in the app.",
};

/** Whether a task's "Tell me when it's done" makes this type alert. */
export const followsTask = (type: AlertType): boolean => type === "finished" || type === "failed" || type === "reply";

/**
 * The type of an item, as `AlertType::of` on the computer works it out: the
 * one it was raised with, or the one its kind, topic and task say.
 * `conversation` is whether its task is a chat. Null for an item that never
 * notifies.
 */
export function alertTypeOf(item: Pick<AttentionItem, "kind" | "topic" | "alert" | "account_id" | "task_id" | "report_id">, conversation: boolean): AlertType | null {
  if (item.alert) return item.alert;
  if (item.account_id && !item.task_id) return "sign_in";
  if (item.report_id) {
    if (item.kind === "question") return "report_question";
    if (!item.task_id) return "reports";
  }
  const topic: AlertTopic | undefined = item.topic;
  switch (item.kind) {
    case "connection_issue": return null;
    case "question": return "question";
    case "approval": return "approval";
    case "conflict": case "reconciliation_required": return "decision";
    case "failure": return "failed";
    case "completed":
      switch (topic) {
        case "merged": return "merged";
        case "pr_ready": return "pr_ready";
        case "issues": return "issues";
        case "reports": return "reports";
        case "pr_needs_you": return "pr_needs_you";
        default: return conversation ? "reply" : "finished";
      }
    case "waiting_for_you":
      switch (topic) {
        case "pr_needs_you": return "pr_needs_you";
        case "issues": return "issue_call";
        case "slow_work": return "slow";
        // A review that couldn't check or file its findings.
        case "finished": case "failed": return "failed";
        case "reports": return "reports";
        case "pr_ready": return "pr_ready";
        case "merged": return "merged";
        default: return "decision";
      }
    default: return null;
  }
}

/** A project's own choice for a type, if it made one. */
export function ownTier(p: ProjectNotifications | undefined, type: AlertType): Tier | null {
  return p?.tiers?.find((t) => t.type === type)?.tier ?? null;
}
/** The project's choices with `tier` for `type`, or none of its own. */
export function withOwnTier(p: ProjectNotifications, type: AlertType, tier: Tier | null): ProjectNotifications {
  const rest: TypeTier[] = (p.tiers ?? []).filter((t) => t.type !== type);
  const tiers = tier ? [...rest, { type, tier }].sort((a, b) => order(a.type) - order(b.type)) : rest;
  const { tiers: _, ...plain } = p;
  return tiers.length ? { ...plain, tiers } : plain;
}
const order = (type: AlertType) => ALERT_TYPES.findIndex((t) => t.type === type);

/**
 * How an item of `type` shows on this device: a task's bell makes its
 * finish, failure or reply alert; otherwise a muted project shows nothing,
 * a project's own choice wins, and this device's choice applies.
 */
export function tierFor(type: AlertType, device: Tier, project?: ProjectNotifications, bell = false): Tier {
  if (bell && followsTask(type)) return "alert";
  if (project?.mute) return "off";
  return ownTier(project, type) ?? device;
}

/** Projects with any choice of their own for a type, with `tiers` where
 * the computer keeps them. */
export function sameTiers(a: ProjectNotifications, b: ProjectNotifications): boolean {
  const x = a.tiers ?? [], y = b.tiers ?? [];
  return x.length === y.length && x.every((t) => y.some((u) => u.type === t.type && u.tier === t.tier));
}

/** One project's choices, said briefly: "Default", "Muted" or "3 changes". */
export function projectSummary(settings: NotificationSettings, projectId: string, typed: boolean): string {
  const p = settings.projects.find((q) => q.project_id === projectId);
  if (!p) return "Default";
  if (p.mute) return "Muted";
  const changes = typed ? (p.tiers?.length ?? 0) + (p.bundle !== undefined ? 1 : 0) : p.on.length + p.off.length + (p.bundle !== undefined ? 1 : 0);
  return changes ? `${changes} ${changes === 1 ? "change" : "changes"}` : "Default";
}
