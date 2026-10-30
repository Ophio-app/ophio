// SPDX-License-Identifier: Apache-2.0
//
// Which alerts reach the person, as the computer keeps them: a default for
// every project, and each project's own choices over it. Read as
// `protocol::notifications` reads them, so this app holds back the same
// alerts as the background watcher.

import type { HostSession } from "../host/session";
import type { AlertTopic, AttentionItem, NotificationSettings, ProjectNotifications } from "../protocol/types";
import { Store } from "../state/store";

/** The computer's word that it keeps which alerts reach the person. */
export const NOTIFICATION_SETTINGS_FEATURE = "notification_settings_v1";

export interface TopicInfo {
  topic: AlertTopic;
  label: string;
  hint: string;
}
/** Every topic, in the order Settings lists them. */
export const TOPICS: readonly TopicInfo[] = [
  { topic: "reports", label: "Problem reports", hint: "A question about your report, a reproduction, or a fix, and new reports people send you." },
  { topic: "question", label: "Questions", hint: "An agent asks you something." },
  { topic: "approval", label: "Approvals", hint: "An agent waits for your leave to do something." },
  { topic: "needs_you", label: "Tasks waiting for you", hint: "To go ahead, settle a conflict, or say how it ended." },
  { topic: "finished", label: "Finished tasks", hint: "A task finished its work." },
  { topic: "failed", label: "Tasks that stopped with a problem", hint: "A task couldn’t finish." },
  { topic: "pr_ready", label: "Pull requests ready to merge", hint: "Checks passed and nothing blocks it." },
  { topic: "pr_needs_you", label: "Pull requests that need you", hint: "A check failed, a conflict, or a review asks for changes." },
  { topic: "merged", label: "Merged pull requests", hint: "A pull request merged." },
  { topic: "issues", label: "Issues", hint: "Issues found and filed, closed as duplicates, or not reproduced." },
  { topic: "slow_work", label: "Slow or stalled work", hint: "A step is taking long, or work stopped for showing no progress." },
];

/** The topic of an item of `kind` that names none of its own, as `AlertTopic::of`. */
export function topicOf(kind: AttentionItem["kind"]): AlertTopic | null {
  switch (kind) {
    case "question": return "question";
    case "approval": return "approval";
    case "waiting_for_you": case "conflict": case "reconciliation_required": return "needs_you";
    case "completed": return "finished";
    case "failure": return "failed";
    default: return null;
  }
}
export const itemTopic = (item: AttentionItem): AlertTopic | null => item.topic ?? topicOf(item.kind);

export const defaultNotificationSettings: NotificationSettings = { off: [], bundle: false, projects: [], revision: 0 };

export function projectChoices(settings: NotificationSettings, projectId: string): ProjectNotifications | undefined {
  return settings.projects.find((p) => p.project_id === projectId);
}
/** Whether an alert about `topic` in `projectId` reaches the person. */
export function alertsFor(settings: NotificationSettings, projectId: string, topic: AlertTopic): boolean {
  const p = projectChoices(settings, projectId);
  if (p && (p.mute || p.off.includes(topic))) return false;
  if (p?.on.includes(topic)) return true;
  return !settings.off.includes(topic);
}
/** Whether `projectId`'s alerts fold into one. */
export function bundlesFor(settings: NotificationSettings, projectId: string): boolean {
  return projectChoices(settings, projectId)?.bundle ?? settings.bundle;
}
/** The projects with `choice` in place of the project's own, leaving out one that says nothing. */
export function withProject(settings: NotificationSettings, choice: ProjectNotifications): ProjectNotifications[] {
  const rest = settings.projects.filter((p) => p.project_id !== choice.project_id);
  const empty = !choice.mute && !choice.on.length && !choice.off.length && choice.bundle === undefined;
  return empty ? rest : [...rest, choice];
}

export type NotificationChoices = Omit<NotificationSettings, "revision">;

/** Whether two sets of choices say the same, as the computer keeps them:
 * each topic once and in its own order. */
export function sameChoices(a: NotificationChoices, b: NotificationChoices): boolean {
  const same = (x: readonly AlertTopic[], y: readonly AlertTopic[]) => {
    const ys = new Set(y);
    return new Set(x).size === ys.size && x.every((t) => ys.has(t));
  };
  if (a.bundle !== b.bundle || !same(a.off, b.off) || a.projects.length !== b.projects.length) return false;
  return a.projects.every((p) => {
    const q = b.projects.find((q) => q.project_id === p.project_id);
    return !!q && p.mute === q.mute && (p.bundle ?? null) === (q.bundle ?? null) && same(p.on, q.on) && same(p.off, q.off);
  });
}

export interface NotificationSettingsState {
  view: NotificationSettings | null;
  error: unknown;
}

/**
 * The computer's settings, read again whenever it says they changed, while
 * anything follows them. The app's own alerts follow them for as long as it
 * is connected.
 */
export class NotificationSettingsStore extends Store<NotificationSettingsState> {
  #followers = 0;
  #stop: (() => void) | null = null;
  #asked = 0;
  constructor(private readonly session: HostSession) {
    super({ view: null, error: null });
  }
  /** The settings to go by: the default until the computer says otherwise. */
  current(): NotificationSettings {
    return this.get().view ?? defaultNotificationSettings;
  }
  supported(): boolean {
    return this.session.state.get().capabilities?.features.includes(NOTIFICATION_SETTINGS_FEATURE) ?? false;
  }
  follow(): () => void {
    if (this.#followers++ === 0) {
      // The revision read on this connection; a new connection reads again.
      let read: number | null = null;
      const check = () => {
        if (this.session.link.get().state !== "online") { read = null; return; }
        const revision = this.session.state.get().notificationsRevision;
        if (read === revision || !this.supported()) return;
        read = revision;
        this.refresh();
      };
      const offState = this.session.state.subscribe(check);
      const offLink = this.session.link.subscribe(check);
      check();
      this.#stop = () => { offState(); offLink(); };
    }
    let followed = true;
    return () => {
      if (!followed) return;
      followed = false;
      if (--this.#followers === 0) { this.#stop?.(); this.#stop = null; }
    };
  }
  refresh(): void {
    const asked = ++this.#asked;
    void this.session.query({ kind: "notification_settings" }).then(
      ({ view }) => { if (asked === this.#asked) this.set({ view, error: null }); },
      (error: unknown) => { if (asked === this.#asked) this.set((s) => ({ ...s, error })); },
    );
  }
}
const stores = new WeakMap<HostSession, NotificationSettingsStore>();
export function notificationSettings(session: HostSession): NotificationSettingsStore {
  let store = stores.get(session);
  if (!store) { store = new NotificationSettingsStore(session); stores.set(session, store); }
  return store;
}
