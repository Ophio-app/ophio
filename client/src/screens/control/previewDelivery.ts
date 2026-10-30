// SPDX-License-Identifier: Apache-2.0
import type { App } from "../../app/context";
import type { Command, CommandReceipt, DeliveryState, TimelineItem } from "../../protocol/types";
import type { HostState } from "../../state/projection";
import { Store } from "../../state/store";
import { clockTime } from "../../design/time";

export interface PreviewDelivery {
  taskId: string; taskTitle: string; at: number;
  request?: Extract<TimelineItem["item"], { kind: "user_message" }>;
  commandId?: string; bundleId?: string; revision?: number;
  afterSeq?: number; messageSeq?: number; state?: DeliveryState;
}
const stores = new Map<string, Store<Record<string, PreviewDelivery>>>();
const watched = new WeakSet<App["session"]>();
const key = (profileId: string) => `preview-deliveries.${profileId}`;
export function previewDeliveries(profileId: string): Store<Record<string, PreviewDelivery>> {
  let store = stores.get(profileId);
  if (!store) {
    let saved: Record<string, PreviewDelivery> = {};
    try { saved = JSON.parse(localStorage.getItem(key(profileId)) ?? "{}"); } catch { /* Keep this session's receipts usable. */ }
    store = new Store(saved ?? {});
    stores.set(profileId, store);
  }
  return store;
}
function save(profileId: string, next: Record<string, PreviewDelivery>): void {
  try { localStorage.setItem(key(profileId), JSON.stringify(next)); } catch { /* Retain the known delivery in memory. */ }
  previewDeliveries(profileId).set(next);
}

/** Loaded host messages take precedence; acceptance is never proof of delivery. */
export function resolvePreviewDelivery(record: PreviewDelivery, host: HostState): PreviewDelivery {
  const message = host.timelines[record.taskId]?.items.findLast(({ seq, item }) =>
    item.kind === "user_message" && seq > (record.afterSeq ?? -1) &&
    !!record.bundleId && item.bundle?.bundle_id === record.bundleId && item.bundle.revision === record.revision &&
    (record.messageSeq === undefined || record.messageSeq === seq));
  if (!message || message.item.kind !== "user_message") return record;
  const state = message.item.delivery;
  const deliveredAt = host.timelines[record.taskId]?.items.findLast(({ item }) =>
    item.kind === "delivery" && item.message_seq === message.seq && item.state === "native_delivered")?.at;
  const at = state === "native_delivered" ? deliveredAt ?? message.at : message.at;
  if (record.state === state && record.messageSeq === message.seq && record.at === at && record.request) return record;
  return { ...record, state, messageSeq: message.seq, at, request: record.request ?? structuredClone(message.item) };
}
export function previewDeliveryText(record: PreviewDelivery, host: HostState): string {
  const title = host.tasks[record.taskId]?.title ?? record.taskTitle;
  switch (record.state) {
    case "native_delivered": return `Sent with markup to ${title} · ${clockTime(record.at)}`;
    case "queued_for_boundary": return `Waiting to send to ${title} · held until its turn ends`;
    case "cancelled": return `Not delivered to ${title} · cancelled`;
    case "failed": return `Not delivered to ${title}${host.tasks[record.taskId]?.state === "interrupted" ? " · task stopped" : " · delivery failed"}`;
    case "host_accepted": return `Handed to ${host.host?.name ?? "the computer"} for ${title} · not yet confirmed by the agent`;
    default: return `Delivery to ${title} is unconfirmed`;
  }
}

/**
 * The picture whose delivery line should take focus once Control shows it
 * again. Starting a task from a preview's markup closes Review, and the
 * control that started it goes with it; the picture's line says where the
 * markup went and how it stands, and offers the task.
 */
const focusRequests = new WeakMap<object, Store<PreviewDeliveryFocus | null>>();
export interface PreviewDeliveryFocus { captureId: string; at: number }
export function previewDeliveryFocus(ui: object): Store<PreviewDeliveryFocus | null> {
  let request = focusRequests.get(ui);
  if (!request) { request = new Store<PreviewDeliveryFocus | null>(null); focusRequests.set(ui, request); }
  return request;
}
/** A request the line didn't take at once (its card wasn't shown) is stale:
 * moving focus later would pull it away from whatever the person is doing. */
export const PREVIEW_FOCUS_FRESH_MS = 2000;

/** The commands that carry a bundle of pictures and notes. */
type Carrying = Extract<Command, { command: "create_task" | "send_message" | "resume_task" }>;
const carrying = (body: Command | null | undefined): body is Carrying =>
  body?.command === "create_task" || body?.command === "send_message" || body?.command === "resume_task";

/** The first picture from a preview on this computer that `body` carries. */
export function previewCaptureIn(body: Command | null | undefined, hostId: string): string | null {
  const bundle = carrying(body) ? body.bundle : null;
  if (!bundle) return null;
  for (const source of bundle.sources)
    if (source.origin.kind === "preview_capture" && source.origin.host_id === hostId) return source.origin.capture_id;
  return null;
}

/** Keep delivery updates even when Control is closed or the timeline is later unloaded. */
export function watchPreviewDeliveries(app: App): void {
  if (watched.has(app.session)) return;
  watched.add(app.session);
  const update = () => {
    const current = previewDeliveries(app.profile.profileId).get();
    const host = app.session.state.get();
    const next = Object.fromEntries(Object.entries(current).map(([id, record]) => [id, resolvePreviewDelivery(record, host)]));
    if (Object.keys(current).some(id => current[id] !== next[id])) save(app.profile.profileId, next);
  };
  app.session.state.subscribe(update);
  update();
}

/** Remember the immutable submitted bundle and follow its message, not its command receipt. */
export function recordPreviewSend(app: App, body: Command | null | undefined, receipt: CommandReceipt, at = Date.now()): void {
  if (!carrying(body) || receipt.status === "rejected" || !body.bundle) return;
  const taskId = "task_id" in body ? body.task_id
    : receipt.result?.kind === "task_created" ? receipt.result.task_id : null;
  if (!taskId) return;
  const host = app.session.state.get();
  const taskTitle = host.tasks[taskId]?.title
    ?? (body.command === "create_task" ? body.title : null) ?? "Task";
  const next = { ...previewDeliveries(app.profile.profileId).get() };
  for (const source of body.bundle.sources) {
    const origin = source.origin;
    if (origin.kind !== "preview_capture" || origin.host_id !== app.profile.hostId) continue;
    const prior = next[origin.capture_id];
    if (prior?.commandId === receipt.command_id) continue;
    next[origin.capture_id] = resolvePreviewDelivery({ taskId, taskTitle, at,
      request: { kind: "user_message", text: body.command === "create_task" ? body.prompt : body.text ?? "", bundle: structuredClone(body.bundle), delivery: "host_accepted" },
      commandId: receipt.command_id, bundleId: body.bundle.bundle_id, revision: body.bundle.revision,
      afterSeq: prior?.taskId === taskId ? prior.messageSeq : undefined, state: "host_accepted",
    }, host);
  }
  save(app.profile.profileId, next);
  watchPreviewDeliveries(app);
}
