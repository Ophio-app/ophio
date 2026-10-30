// SPDX-License-Identifier: Apache-2.0
import type { Platform, LocalNotification, NotificationPermission } from "../host/link";
import type { HostSession, PendingCommand } from "../host/session";
import type { AttentionItem, HarnessKind } from "../protocol/types";
import type { HostState } from "../state/projection";
import type { UiStore } from "../app/ui";
import { Store } from "../state/store";
import { appLock } from "./lock";
import { reportPlatformError } from "./errors";
import { backgroundAlerts } from "./alerts";
import { alertsFor, bundlesFor, itemTopic, notificationSettings } from "./notificationSettings";

export type NotificationDetail = "minimal" | "task_title";
const KEY = "notification-detail";
function load(): NotificationDetail {
  try {
    return localStorage.getItem(KEY) === "task_title"
      ? "task_title"
      : "minimal";
  } catch {
    return "minimal";
  }
}
export const notificationDetail = new Store<NotificationDetail>(load());
export function setNotificationDetail(value: NotificationDetail): void {
  try {
    localStorage.setItem(KEY, value);
    notificationDetail.set(value);
  } catch {
    reportPlatformError({
      operation: "Notifications",
      message:
        "Notification detail couldn’t be saved. Free some device storage and try again.",
    });
  }
}

export interface AttentionRoute {
  kind: "attention";
  profileId: string;
  attentionId: string;
}
export function parseAttentionRoute(value: unknown): AttentionRoute | null {
  if (!value || typeof value !== "object") return null;
  const r = value as Record<string, unknown>;
  if (
    r.kind !== "attention" ||
    typeof r.profileId !== "string" ||
    !r.profileId ||
    r.profileId.length > 256 ||
    typeof r.attentionId !== "string" ||
    !r.attentionId ||
    r.attentionId.length > 256
  )
    return null;
  return {
    kind: "attention",
    profileId: r.profileId,
    attentionId: r.attentionId,
  };
}
/** The note that a computer offers an update to this app. */
export interface AppUpdateRoute {
  kind: "app_update";
  profileId: string;
}
/** The note that counts a project's alerts: it leads to the project's Needs you. */
export interface NeedsYouRoute {
  kind: "needs_you";
  profileId: string;
  projectId: string;
}
export interface ReportRoute { kind: "report"; profileId: string; reportId: string }
export type OpenRoute = AttentionRoute | AppUpdateRoute | NeedsYouRoute | ReportRoute;
const named = (v: unknown): v is string => typeof v === "string" && !!v && v.length <= 256;
/** Where a notification leads: an attention item, a project's Needs you, or Versions for an update. */
export function parseOpenRoute(value: unknown): OpenRoute | null {
  if (value && typeof value === "object") {
    const r = value as Record<string, unknown>;
    if (r.kind === "report" && named(r.profileId) && named(r.reportId)) return { kind: "report", profileId: r.profileId, reportId: r.reportId };
    if (r.kind === "app_update" && named(r.profileId))
      return { kind: "app_update", profileId: r.profileId };
    if (r.kind === "needs_you" && named(r.profileId) && named(r.projectId))
      return { kind: "needs_you", profileId: r.profileId, projectId: r.projectId };
  }
  return parseAttentionRoute(value);
}
/** The tag of the note that counts a project's alerts, as the background watcher's. */
export const foldedTag = (profileId: string, projectId: string) => `${profileId}/project:${projectId}`;
/** The note that counts a project's alerts, each saying whether it needs the person. */
export function foldedNotification(
  computer: { profileId: string; name: string },
  project: { project_id: string; name: string },
  members: Iterable<boolean>,
): LocalNotification {
  const all = [...members];
  const needsYou = all.filter(Boolean).length;
  const other = all.length - needsYou;
  const parts = [...(needsYou ? [`${needsYou} need you`] : []), ...(other ? [`${other} to look at`] : [])];
  return {
    id: foldedTag(computer.profileId, project.project_id),
    title: `${project.name} has ${all.length} notification${all.length === 1 ? "" : "s"}`.slice(0, 256),
    body: [computer.name, ...(parts.length ? [parts.join(", ")] : [])].join(" · ").slice(0, 2048),
    category: needsYou ? "needs_you" : "finished",
    route: { kind: "needs_you", profileId: computer.profileId, projectId: project.project_id } satisfies NeedsYouRoute,
  };
}
/** As the background watcher names them, so an alert both raise reads the same. */
const AGENTS: Record<HarnessKind, string> = { claude_code: "Claude Code", codex: "Codex", hermes: "Hermes", open_claw: "OpenClaw", pi: "Pi" };
function alertKind(kind: AttentionItem["kind"], harness: HarnessKind): { category: "needs_you" | "finished"; title: string } | null {
  switch (kind) {
    case "question": return { category: "needs_you", title: `Question from ${AGENTS[harness] ?? "the agent"}` };
    case "approval": return { category: "needs_you", title: "Approval waiting" };
    case "waiting_for_you": case "conflict": case "reconciliation_required": return { category: "needs_you", title: "Task needs you" };
    case "completed": return { category: "finished", title: "Task finished" };
    case "failure": return { category: "finished", title: "Task stopped with a problem" };
    default: return null;
  }
}
export function notificationFor(
  item: AttentionItem,
  state: HostState,
  computer: { profileId: string; name: string },
  detail: NotificationDetail,
): LocalNotification | null {
  if (item.report_id && item.resolved_at === null && state.capabilities?.features.includes("reports_v1")) return { id: `${computer.profileId}/${item.attention_id}`, title: "Your problem report", body: "There’s an update on your report.", category: item.kind === "question" ? "needs_you" : "finished", route: { kind: "report", profileId: computer.profileId, reportId: item.report_id } satisfies ReportRoute };
  const task = item.task_id ? state.tasks[item.task_id] : undefined;
  const project = task ? state.projects[task.project_id] : undefined;
  if (item.resolved_at !== null || !project || !task) return null;
  const alert = alertKind(item.kind, task.harness);
  if (!alert) return null;
  return {
    id: `${computer.profileId}/${item.attention_id}`,
    title: alert.title,
    body: [computer.name, project.name, ...(detail === "task_title" ? [task.title] : [])].join(" · ").slice(0, 2048),
    category: alert.category,
    ...(alert.category === "finished" ? { group: `${computer.profileId}:finished`, groupTitle: computer.name.slice(0, 256) } : {}),
    route: {
      kind: "attention",
      profileId: computer.profileId,
      attentionId: item.attention_id,
    } satisfies AttentionRoute,
  };
}
/** Tasks and conversations open on the screen, by computer: their alerts
 * leave the shade as they open, answered or not, as the person has seen
 * them. What still needs an answer stays under Needs you. */
const opened = new Map<string, Map<string, number>>();
const openedChanged = new Set<() => void>();
export function viewingTask(profileId: string, taskId: string): () => void {
  const tasks = opened.get(profileId) ?? new Map<string, number>();
  opened.set(profileId, tasks);
  tasks.set(taskId, (tasks.get(taskId) ?? 0) + 1);
  for (const f of openedChanged) f();
  return () => {
    const n = (tasks.get(taskId) ?? 1) - 1;
    if (n > 0) tasks.set(taskId, n);
    else tasks.delete(taskId);
  };
}

/** Snapshot is a baseline, not a burst of new notifications. Updates and replay
 * of an existing identity never notify twice, including when seen in foreground.
 * An item answered anywhere clears its alert, whichever side raised it.
 * Alerts the person turned off, and quiet ones, count as seen and never show, and a project
 * whose alerts fold shows its second and later as one note that counts them,
 * as the background watcher does. */
export function watchAttention(
  platform: Platform,
  session: HostSession,
  computer: string,
): () => void {
  if (!platform.notify) return () => {};
  const access = notificationAccess(platform);
  const offAccess = access.start();
  const watcher = backgroundAlerts(platform);
  const offWatcher = watcher?.start();
  watcher?.viewing(session.profileId, true);
  const lock = appLock(platform);
  const settings = notificationSettings(session);
  const offSettings = settings.follow();
  // Alerts this app raised and are still open, by project, each saying
  // whether it needs the person; and the projects among them that show as
  // one note that counts them.
  const raised = new Map<string, Map<string, boolean>>();
  const folded = new Set<string>();
  const show = (notification: LocalNotification, name: string, cancel: string[] = []) => {
    void (async () => {
      try {
        // Cleared first, so a note shown again alerts again.
        if (cancel.length) await platform.cancelNotifications?.(cancel).catch(() => {});
        // Where the watcher keeps a record, it records this as raised,
        // and refuses once Android's shade has no room.
        await platform.notify?.(notification);
      } catch {
        const permission = await access.refresh();
        access.missed(session.profileId, name, permission === "promptable" || permission === "blocked");
      }
    })();
  };
  let primed = false;
  // Kept by id for good: a resolved item is never reopened under its id;
  // one raised again comes as a new item.
  const seen = new Set<string>();
  const cleared = new Set<string>();
  // Told to the watcher as shown on the screen.
  const reported = new Set<string>();
  // Every item of a record already walked has been seen; most updates, such
  // as streamed text, leave the attention record as it was. The screen
  // coming into view, or unlocking, walks it again to report what it shows.
  let walked: HostState["attention"] | null = null;
  let walkedResolved: HostState["resolvedAttention"] | null = null;
  const changed = (again = false) => {
    const state = session.state.get();
    if (!state.caughtUp || (!again && state.attention === walked && state.resolvedAttention === walkedResolved)) return;
    walked = state.attention;
    walkedResolved = state.resolvedAttention;
    const initial = !primed;
    primed = true;
    // The lock screen hides the app's own: only what it shows unlocked counts.
    const visible = document.visibilityState !== "hidden" && !lock.get().locked;
    const answered: string[] = [];
    const shown: string[] = [];
    const recount = new Set<string>();
    const clear = (id: string) => {
      if (cleared.has(id)) return;
      cleared.add(id);
      answered.push(`${session.profileId}/${id}`);
      for (const [projectId, members] of raised) if (members.delete(id)) recount.add(projectId);
    };
    const answer = (id: string) => {
      seen.add(id);
      reported.delete(id);
      clear(id);
    };
    const here = { profileId: session.profileId, name: computer };
    const open = visible ? opened.get(session.profileId) : undefined;
    for (const id of Object.keys(state.resolvedAttention)) answer(id);
    for (const item of Object.values(state.attention)) {
      if (item.resolved_at !== null) { answer(item.attention_id); continue; }
      if (item.task_id && open?.has(item.task_id)) {
        // Open on the screen: seen, so it leaves the shade and is never raised.
        seen.add(item.attention_id);
        if (!reported.has(item.attention_id)) {
          reported.add(item.attention_id);
          shown.push(item.attention_id);
        }
        clear(item.attention_id);
        continue;
      }
      // On screen the app shows it, and tells the watcher, which would
      // otherwise raise it once the app leaves the screen.
      if (visible && !reported.has(item.attention_id)) {
        reported.add(item.attention_id);
        shown.push(item.attention_id);
      }
      if (seen.has(item.attention_id)) continue;
      seen.add(item.attention_id);
      // Part of something that alerts once, as a whole: seen, never raised.
      if (visible || initial || item.quiet) continue;
      // The watcher raises it, even if Android freezes this page first.
      if (watcher?.covers(session.profileId)) continue;
      if (item.report_id) {
        if (!settings.current().off.includes("reports")) {
          const notification = notificationFor(item, state, here, "minimal");
          if (notification) show(notification, "Problem report update");
        }
        continue;
      }
      const project = item.task_id ? state.projects[state.tasks[item.task_id]?.project_id ?? ""] : undefined;
      const topic = itemTopic(item);
      if (!project || (topic && !alertsFor(settings.current(), project.project_id, topic))) continue;
      const notification = notificationFor(item, state, here, lock.get().enabled ? "minimal" : notificationDetail.get());
      if (!notification) continue;
      // The banner already sits under this computer, so it names only the work.
      const name = notification.body.startsWith(`${computer} · `) ? notification.body.slice(computer.length + 3) : notification.body;
      const members = raised.get(project.project_id) ?? new Map<string, boolean>();
      raised.set(project.project_id, members);
      const others = [...members.keys()];
      const needsYou = notification.category === "needs_you";
      members.set(item.attention_id, needsYou);
      if (!others.length || !bundlesFor(settings.current(), project.project_id)) { show(notification, name); continue; }
      // A second alert in a project whose alerts fold: one note counts them,
      // in place of their own, and alerts again only for what needs the person.
      const note = foldedNotification(here, project, members.values());
      const was = folded.has(project.project_id);
      folded.add(project.project_id);
      recount.delete(project.project_id);
      show(note, project.name, was ? (needsYou ? [note.id] : []) : others.map((id) => `${session.profileId}/${id}`));
    }
    // A note that counts a project's alerts counts again as they're answered,
    // and goes once none is left.
    for (const projectId of recount) {
      const members = raised.get(projectId);
      const project = state.projects[projectId];
      if (!members?.size) raised.delete(projectId);
      if (!folded.has(projectId)) continue;
      if (members?.size && project) { void Promise.resolve(platform.notify?.(foldedNotification(here, project, members.values()))).catch(() => {}); continue; }
      folded.delete(projectId);
      answered.push(foldedTag(session.profileId, projectId));
    }
    for (let i = 0; i < answered.length; i += 256)
      void platform.cancelNotifications?.(answered.slice(i, i + 256)).catch(() => {});
    for (let i = 0; i < shown.length; i += 200)
      watcher?.seen(session.profileId, shown.slice(i, i + 200));
  };
  changed();
  const off = session.state.subscribe(() => changed());
  const onOpened = () => changed(true);
  openedChanged.add(onOpened);
  let locked = lock.get().locked;
  const offLock = lock.subscribe(() => {
    if (lock.get().locked === locked) return;
    locked = lock.get().locked;
    changed(true);
  });
  const onVisibility = () => changed(true);
  document.addEventListener("visibilitychange", onVisibility);
  return () => {
    off(); offLock(); openedChanged.delete(onOpened); document.removeEventListener("visibilitychange", onVisibility); offAccess(); offSettings();
    watcher?.viewing(session.profileId, false);
    offWatcher?.();
  };
}
/** Only selects an object. The destination handles missing/settled references. */
export function openNotification(
  route: AttentionRoute,
  profileId: string,
  ui: UiStore,
): boolean {
  if (route.profileId !== profileId) return false;
  ui.push({ screen: "attention", attentionId: route.attentionId }, "work");
  return true;
}

export interface MissedAlerts {
  count: number;
  names: string[];
  off: boolean;
}
interface NotificationState {
  permission: NotificationPermission | null;
  pending: boolean;
  error: string | null;
  missed: Record<string, MissedAlerts | undefined>;
}

/** Permission is device-wide; missed alerts and the contextual choice stay per computer. */
export class NotificationAccess extends Store<NotificationState> {
  #users = 0;
  #stop: (() => void) | null = null;
  #read = 0;

  constructor(readonly platform: Platform) {
    super({ permission: null, pending: false, error: null, missed: {} });
  }

  start(): () => void {
    if (this.#users++ === 0) {
      const visible = () => { if (document.visibilityState === "visible") void this.refresh(); };
      document.addEventListener("visibilitychange", visible);
      const off = this.platform.onForeground?.(() => { void this.refresh(); });
      void this.refresh();
      this.#stop = () => { document.removeEventListener("visibilitychange", visible); off?.(); };
    }
    return () => { if (--this.#users === 0) { this.#stop?.(); this.#stop = null; } };
  }

  async refresh(): Promise<NotificationPermission | null> {
    if (!this.platform.notificationPermission) return null;
    const read = ++this.#read;
    try {
      const permission = await this.platform.notificationPermission();
      if (!["granted", "promptable", "blocked"].includes(permission)) throw new Error("Invalid permission state");
      if (read === this.#read) this.set((s) => ({ ...s, permission, error: null }));
      return permission;
    } catch {
      if (read === this.#read) this.set((s) => ({ ...s, permission: null, error: "This device couldn’t check whether task alerts are allowed." }));
      return null;
    }
  }

  async allow(): Promise<void> {
    if (this.get().pending) return;
    const permission = this.get().permission;
    const action = permission === "blocked" ? this.platform.openNotificationSettings : this.platform.requestNotifications;
    if (!action) return;
    this.set((s) => ({ ...s, pending: true, error: null }));
    let error: string | null = null;
    try {
      await action.call(this.platform);
    } catch {
      error = permission === "blocked"
        ? "Android’s notification settings couldn’t be opened. Try again."
        : "Task alerts weren’t enabled. You can try again in Settings › Notifications.";
    } finally {
      await this.refresh();
      this.set((s) => ({ ...s, pending: false, error: error ?? s.error }));
    }
  }

  missed(profileId: string, name: string, off: boolean): void {
    this.set((s) => {
      const before = s.missed[profileId];
      return { ...s, missed: { ...s.missed, [profileId]: {
        count: (before?.count ?? 0) + 1,
        names: [...new Set([...(before?.names ?? []), name])].slice(0, 3),
        off: off && (before?.off ?? true),
      } } };
    });
  }

  dismissMissed(profileId: string): void {
    this.set((s) => ({ ...s, missed: { ...s.missed, [profileId]: undefined } }));
  }
}

const permissions = new WeakMap<Platform, NotificationAccess>();
export function notificationAccess(platform: Platform): NotificationAccess {
  let access = permissions.get(platform);
  if (!access) { access = new NotificationAccess(platform); permissions.set(platform, access); }
  return access;
}

export function missedAlertText(missed: MissedAlerts, permission: NotificationPermission | null = null): string {
  const names = missed.names.map((name) => `“${name}”`).join(", ");
  const subject = missed.count === 1 ? `An alert for ${names}` : `${missed.count} alerts, including ${names},`;
  const reason = missed.off ? ` because alerts ${permission === "granted" ? "were" : "are"} off` : " on this device";
  return `${subject} couldn’t be shown${reason}.`;
}

export interface AlertContext { projectId: string; taskId: string }

/** Only accepted commands issued during this visit qualify. A snapshot, replay
 * or an automatic queue dispatch never becomes a permission offer. */
export class TaskAlertsOffer extends Store<AlertContext | null> {
  #offered = false;
  #key: string;
  constructor(profileId: string) {
    super(null);
    this.#key = `task-alerts-offer.${profileId}`;
    try { this.#offered = localStorage.getItem(this.#key) === "seen"; } catch { /* Remember in this session. */ }
  }

  shown(): void {
    if (this.#offered) return;
    this.#offered = true;
    try { localStorage.setItem(this.#key, "seen"); }
    catch { reportPlatformError({ operation: "Task alerts", message: "Your alert choice couldn’t be saved on this device. It may be offered again after a restart." }); }
  }

  dismiss(): void { this.shown(); this.set(null); }

  watch(session: HostSession, access: NotificationAccess): () => void {
    let watching = true;
    const seen = new Set(Object.keys(session.commands.get()));
    const changed = () => {
      if (this.#offered || this.get()) return;
      for (const command of Object.values(session.commands.get())) {
        if (seen.has(command.id) || command.phase === "sending") continue;
        seen.add(command.id);
        if ((command.phase !== "accepted" && command.phase !== "duplicate") || document.visibilityState !== "visible") continue;
        const context = alertContext(command, session.state.get());
        if (!context) continue;
        void access.refresh().then((permission) => {
          if (watching && !this.#offered && !this.get() && permission === "promptable" && document.visibilityState === "visible") this.set(context);
        });
      }
    };
    const off = session.commands.subscribe(changed);
    return () => { watching = false; off(); };
  }
}

function alertContext(command: PendingCommand, state: HostState): AlertContext | null {
  const body = command.body;
  const result = command.receipt?.result;
  if (body.command === "create_task" && body.placement.mode === "start_now" && result?.kind === "task_created") {
    return { projectId: body.project_id, taskId: result.task_id };
  }
  let taskId: string | undefined;
  if (body.command === "resume_task" || body.command === "send_message") taskId = body.task_id;
  if (body.command === "queue_start") taskId = Object.values(state.queues).flatMap((q) => q.items).find((i) => i.item_id === body.item_id)?.task_id;
  const task = taskId ? state.tasks[taskId] : undefined;
  return task ? { projectId: task.project_id, taskId: task.task_id } : null;
}

const offers = new WeakMap<HostSession, TaskAlertsOffer>();
export function taskAlertsOffer(session: HostSession): TaskAlertsOffer {
  let offer = offers.get(session);
  if (!offer) { offer = new TaskAlertsOffer(session.profileId); offers.set(session, offer); }
  return offer;
}
