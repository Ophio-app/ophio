// SPDX-License-Identifier: Apache-2.0
import type { Route } from "../app/ui";
import type { HostProfile } from "../host/link";
import { CommandError, type HostSession } from "../host/session";
import type { TaskRecords } from "../protocol/types";
import { readsHistory } from "../state/projection";
import { taskBadge } from "../screens/talk/TaskBar";

/** Shared with Android and the channel adapters. */
export const OBJECT_LINK_PREFIX = "ophio-object://open/v1/";
/** Links made before the product was named. They may sit in notes and chat
 * history, so they still open. */
export const EARLIER_OBJECT_LINK_PREFIX = "companion-object://open/v1/";
export type ObjectLink = { hostId: string } & (
  | { kind: "task" | "approval" | "attention"; id: string }
  | { kind: "file"; id: string; path: string }
);
const ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
export function safeRelativePath(path: string): boolean {
  return path.length > 0 && path.length <= 2048 && !/[\\:\u0000-\u001f\u007f]/.test(path)
    && path.split("/").every((part) => part !== "" && part !== "." && part !== "..");
}
export function parseObjectLink(value: string): ObjectLink | null {
  const prefix = [OBJECT_LINK_PREFIX, EARLIER_OBJECT_LINK_PREFIX].find((p) => value.startsWith(p));
  if (value.length > 4096 || !prefix || value.includes("#")) return null;
  const [base, query, extra] = value.slice(prefix.length).split("?");
  if (!base || extra !== undefined) return null;
  const parts = base.split("/");
  if (parts.length !== 3) return null;
  const [hostId, kind, id] = parts;
  if (!hostId || !id || !ID.test(hostId) || !ID.test(id)) return null;
  if (kind === "file") {
    if (!query || /%(?![0-9a-f]{2})/i.test(query)) return null;
    try { decodeURIComponent(query.replaceAll("+", " ")); } catch { return null; }
    const q = new URLSearchParams(query), path = q.get("path");
    if ([...q].length !== 1 || !path || !safeRelativePath(path)) return null;
    return { hostId, kind, id, path };
  }
  if (query !== undefined || !["task", "approval", "attention"].includes(kind ?? "")) return null;
  return { hostId, kind: kind as "task" | "approval" | "attention", id };
}
/** Why a reference can't be opened, in words for the person, or null when it can. */
export function objectLinkProblem(value: string): string | null {
  if (parseObjectLink(value)) return null;
  if (![OBJECT_LINK_PREFIX, EARLIER_OBJECT_LINK_PREFIX].some((p) => value.startsWith(p)))
    return `Item links start with ${OBJECT_LINK_PREFIX}. Copy the whole link from the chat message.`;
  return "This item link is incomplete or has been changed. Copy the whole link from the chat message again.";
}
export function objectLink(value: ObjectLink): string {
  return `${OBJECT_LINK_PREFIX}${value.hostId}/${value.kind}/${value.id}${value.kind === "file" ? `?path=${encodeURIComponent(value.path)}` : ""}`;
}
/** Pairing again keeps the earlier pairing, so one computer can have two
 * profiles here; the one already in use is preferred. */
export function pairedComputer(link: ObjectLink, profiles: HostProfile[], preferred?: string | null): HostProfile | null {
  const matching = profiles.filter((p) => p.hostId === link.hostId);
  return matching.find((p) => p.profileId === preferred) ?? matching[0] ?? null;
}
/** A device keeps one pairing per computer: a new one replaces any earlier. */
export function withPairing(profiles: HostProfile[], next: HostProfile): HostProfile[] {
  return [...profiles.filter((p) => p.hostId !== next.hostId), next];
}
export type LinkResolution =
  | { state: "ready"; label: string; route: Route }
  | { state: "settled"; label: string; detail: string; route: Route | null }
  | { state: "missing" | "denied"; detail: string };

/** All object labels come from a fresh grant-filtered host reply, never from the URL. */
export async function resolveObject(link: ObjectLink, session: Pick<HostSession, "query" | "state">): Promise<LinkResolution> {
  const snap = await session.query({ kind: "snapshot" });
  if (snap.host.host_id !== link.hostId) return { state: "denied", detail: "The connected computer doesn’t match this link." };
  const grants = session.state.get().grants;
  const allowed = link.kind === "file" ? grants?.library_read : link.kind === "approval" ? grants?.approvals : grants?.tasks;
  if (!allowed) return { state: "denied", detail: "This device isn’t allowed to view this item on the computer." };
  const missing: LinkResolution = { state: "missing", detail: "This item no longer exists or is no longer shared with this device." };
  if (link.kind === "file") {
    try {
      await session.query({ kind: "library_read", root_id: link.id, path: link.path });
      return { state: "ready", label: `Library file ‘${link.path}’`, route: { screen: "file", rootId: link.id, path: link.path } };
    } catch (e) {
      if (e instanceof CommandError && e.error.code === "not_found") return missing;
      if (e instanceof CommandError && ["permission_denied", "path_not_allowed"].includes(e.error.code))
        return { state: "denied", detail: "The computer hasn’t granted access to this Library file." };
      throw e;
    }
  }
  if (link.kind === "task") {
    const older = !snap.tasks.some((t) => t.task_id === link.id) && readsHistory(session.state.get());
    const records = older ? await olderTask(session, link.id) : snap;
    const task = records?.tasks.find((t) => t.task_id === link.id);
    if (!records || !task) return missing;
    const route: Route = { screen: "receipt", taskId: task.task_id };
    if (["completed", "failed", "interrupted", "archived"].includes(task.state))
      return { state: "settled", label: `task ‘${task.title}’`, detail: `Task status: ${taskBadge(task, records.runs.find(r => r.run_id === task.current_run)).label}. Opening its receipt won’t restart it.`, route };
    return { state: "ready", label: `task ‘${task.title}’`, route };
  }
  if (link.kind === "attention") {
    const item = snap.attention.find((a) => a.attention_id === link.id);
    if (!item) return missing;
    const route: Route = { screen: "attention", attentionId: item.attention_id };
    return item.resolved_at !== null
      ? { state: "settled", label: `item ‘${item.title}’`, detail: "Already answered on the computer.", route }
      : { state: "ready", label: `item ‘${item.title}’`, route };
  }
  const approval = snap.approvals.find((a) => a.approval_id === link.id);
  if (!approval) return missing;
  const item = snap.attention.find((a) => a.approval_id === approval.approval_id);
  const route: Route | null = item ? { screen: "attention", attentionId: item.attention_id } : null;
  if (approval.state !== "pending") return { state: "settled", label: `approval ‘${approval.title}’`, detail: `Already answered: ${approval.state.replaceAll("_", " ")}.`, route };
  // No synthetic attention record: a host without a destination must say so.
  return route ? { state: "ready", label: `approval ‘${approval.title}’`, route }
    : { state: "settled", label: `approval ‘${approval.title}’`, detail: "The request is pending, but the computer hasn’t supplied a destination. Open Needs you after reconnecting.", route: null };
}

/** A task the snapshot leaves out, being older than those it carries. */
async function olderTask(session: Pick<HostSession, "query">, taskId: string): Promise<TaskRecords | null> {
  try {
    return (await session.query({ kind: "task", task_id: taskId })).records;
  } catch (e) {
    if (e instanceof CommandError && e.error.code === "not_found") return null;
    throw e;
  }
}

// A pasted reference follows the same local-lock and preview path as ACTION_VIEW.
const intake = new Set<(url: string) => void>();
export function acceptObjectLink(url: string): void { for (const receive of intake) receive(url); }
export function onObjectLink(receive: (url: string) => void): () => void { intake.add(receive); return () => { intake.delete(receive); }; }
