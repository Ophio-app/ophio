// SPDX-License-Identifier: Apache-2.0
//
// Settings › Notifications. Two levels, each where it belongs: how each type
// shows (Alert, Silent or Off) is this device's, one choice for every
// computer it is paired with; a project's own choices are kept on the
// computer that owns the project, for every device paired with it. Every
// change shows on the tap and takes effect at once.

import { useEffect, useId, useRef, useState, type ReactNode, type RefObject } from "react";
import { useCommandControl } from "../../app/commandControl";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { useComputerEntries, type ComputerEntry } from "../../app/fleet";
import { OnComputer } from "../../app/onComputer";
import { Button } from "../../design/Button";
import { Segmented, type SegmentOption } from "../../design/Chip";
import { onIPhone } from "../../design/formFactor";
import { ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { shallowEqual, useStore } from "../../state/store";
import type { AlertStatus } from "../../host/link";
import type { AlertTopic, AlertType, ProjectNotifications, Tier } from "../../protocol/types";
import { Screen } from "../../shell/Screen";
import {
  NOTIFICATION_SETTINGS_FEATURE,
  TOPICS,
  defaultNotificationSettings,
  sameChoices,
  type NotificationChoices,
  notificationSettings,
  projectChoices,
  withProject,
} from "../../platform/notificationSettings";
import {
  ALERT_TYPES,
  ALERT_TYPES_FEATURE,
  GROUPS,
  TIER_HINTS,
  TIER_LABELS,
  ownTier,
  projectSummary,
  recommendedTiers,
  withOwnTier,
  type AlertTypeInfo,
} from "../../platform/alertTypes";
import { deviceTiers, type Tiers } from "../../platform/deviceTiers";
import {
  notificationDetail,
  setNotificationDetail,
  type NotificationDetail,
} from "../../platform/notifications";
import { backgroundAlerts, type Alerts } from "../../platform/alerts";
import { RELEASE_CHECK_FEATURE } from "./useReleaseCheck";
import { AlertPermissionAction, useNotificationAccess } from "../../shell/AlertsOffer";
import "./notifications.css";

const TIERS: readonly Tier[] = ["alert", "silent", "off"];

export function Notifications() {
  const delivery = useRef<HTMLDetailsElement>(null);
  const deliveryId = useId();
  const openDelivery = () => {
    if (!delivery.current) return;
    delivery.current.open = true;
    const summary = delivery.current.querySelector("summary");
    summary?.focus({ preventScroll: true });
    delivery.current.scrollIntoView({ block: "start" });
  };
  return (<>
    <DeliveryWarning open={openDelivery} detailsId={deliveryId} />
    <WhatNotifies />
    <PhoneStatus deliveryRef={delivery} detailsId={deliveryId} />
    <Projects />
    <HowTheyLook />
    <Elsewhere />
  </>);
}

type DeliveryLink = { open: () => void; detailsId: string };

/** Surface a delivery limit before the choices, without repeating setup instructions. */
function DeliveryWarning(link: DeliveryLink) {
  const { platform } = useApp();
  const { permission } = useNotificationAccess();
  const watcher = backgroundAlerts(platform);
  const limit = !platform.notify ? "This build can’t show system notifications."
    : platform.notificationPermission && permission !== "granted"
      ? permission === "blocked" ? "Phone settings block notifications."
        : permission === "promptable" ? "Phone notifications are off."
          : "Notification permission is unknown. Alerts may not arrive."
      : !watcher ? "Notifications arrive only while Øphio is open." : null;
  if (limit) return <DeliveryLimit {...link} limit={limit} />;
  return watcher ? <WatcherWarning {...link} watcher={watcher} /> : null;
}

function WatcherWarning({ watcher, ...link }: DeliveryLink & { watcher: Alerts }) {
  const view = useStore(watcher, (s) => s, shallowEqual);
  const limit = backgroundDeliveryLimit(view);
  return limit ? <DeliveryLimit {...link} limit={limit} /> : null;
}

function DeliveryLimit({ limit, open, detailsId }: DeliveryLink & { limit: string }) {
  return <aside className="notif-delivery-warning" aria-label="Notification delivery limits">
    <p aria-live="polite">{limit}</p>
    <Button size="sm" variant="quiet" aria-controls={detailsId} onClick={open}>Delivery settings</Button>
  </aside>;
}

/** Whether this phone may show notifications, whether they keep coming once
 * Øphio is closed, and how the watcher's connection to each computer is. */
function PhoneStatus({ deliveryRef, detailsId }: { deliveryRef: RefObject<HTMLDetailsElement | null>; detailsId: string }) {
  const { platform } = useApp();
  const { access, permission, error: permissionError } = useNotificationAccess();
  const watcher = backgroundAlerts(platform);
  const permissionTitle = permission === "granted" ? "Alerts are on" : permission === "promptable" ? "Alerts are off" : permission === "blocked" ? "Alerts are blocked in the phone’s settings" : permissionError ? "Task alert permission is unknown" : "Checking task alerts…";
  return <Section title="This phone" id="settings-notifications">
    <details className="settings-card notif-delivery" ref={deliveryRef} id={detailsId}>
      <summary>
        <span className="notif-delivery__title">Background delivery and phone settings</span>
        <span className="notif-delivery__status">
          {platform.notificationPermission && <span>{permissionTitle}</span>}
          {!platform.notify ? <span>System notifications aren’t available in this build.</span>
            : watcher ? <BackgroundDeliverySummary watcher={watcher} />
              : <span>Notifications arrive only while Øphio is open.</span>}
        </span>
      </summary>
      <div className="settings-stack notif-delivery__body">
      {platform.notificationPermission && <Notice
        tone={permission === "blocked" ? "warn" : permissionError ? "unknown" : "info"}
        title={permissionTitle}
        action={!permission && permissionError ? <Button onClick={() => void access.refresh()}>Check alerts again</Button> : <AlertPermissionAction />}>
        {permissionError}
      </Notice>}
      {!platform.notify ? (
        <Notice>
          System notifications aren’t available in this build. Needs you stays
          available inside the app.
        </Notice>
      ) : watcher ? <ClosedAlerts watcher={watcher} /> : (
        <p className="settings-meta">
          {onIPhone()
            ? "On iPhone, Øphio can notify you only while it’s open. Your choices above apply then."
            : "In this build, notifications arrive only while the app is open. Your choices above apply then."}
        </p>
      )}
      </div>
    </details>
  </Section>;
}

/** Keep delivery limits visible even while the explanation is folded away.
 * ClosedAlerts stays mounted inside the details, so it still follows Android. */
function BackgroundDeliverySummary({ watcher }: { watcher: Alerts }) {
  const view = useStore(watcher, (s) => s, shallowEqual);
  return <span>{backgroundDeliveryLimit(view) ?? "Background delivery is on."}</span>;
}

function backgroundDeliveryLimit({ state, error }: ReturnType<Alerts["get"]>): string | null {
  return error ? "Background delivery status is unavailable."
    : !state ? "Checking background delivery…"
      : !state.settings?.enabled ? "Background delivery is off. Notifications stop when Øphio closes."
        : state.android && !state.android.running ? "Android stopped background notifications."
          : state.android && !state.android.unrestricted ? "Background delivery stops when Øphio closes."
            : Object.values(state.statuses).some((status) => status !== "watching")
              ? "Some computers aren’t delivering background notifications."
              : null;
}

/** How the watcher's own connection to one computer stands, in a few words. */
function statusLine(status: AlertStatus | undefined): { text: string; tone: "ok" | "wait" | "warn" } {
  switch (status) {
    case "watching": return { text: "Connected", tone: "ok" };
    case "update_needed": return { text: "Needs an Øphio update to notify you", tone: "warn" };
    case "unreachable": return { text: "Out of reach, trying again", tone: "wait" };
    case "not_allowed": return { text: "No longer accepts this phone. Pair it again.", tone: "warn" };
    default: return { text: "Connecting…", tone: "wait" };
  }
}

/** "Notify me when Øphio is closed", the battery step, and each computer. */
function ClosedAlerts({ watcher }: { watcher: Alerts }) {
  const { profiles, profile } = useApp();
  const detail = useStore(notificationDetail, (s) => s);
  const { state, pending, error } = useStore(watcher, (s) => s, shallowEqual);
  useEffect(() => watcher.follow(), [watcher]);
  // The switch shows the choice on the tap; Android's answer follows.
  const [asked, setAsked] = useState<boolean | null>(null);
  const on = asked ?? !!state?.settings?.enabled;
  const android = state?.android;
  const turn = (enabled: boolean) => {
    setAsked(enabled);
    void watcher.configure({ enabled, detail }).finally(() => setAsked((a) => (a === enabled ? null : a)));
  };
  const computers = profiles.some((p) => p.profileId === profile.profileId) ? profiles : [profile, ...profiles];
  return <>
    <label className="settings-check">
      <input type="checkbox" checked={on} disabled={!state}
        onChange={(e) => turn(e.target.checked)} />
      Notify me when Øphio is closed
    </label>
    <p className="settings-meta">
      {!on
        ? "Off: notifications come only while Øphio is open or in the background."
        : android && !android.unrestricted
          ? "Keeps a light connection to each paired computer while Øphio runs. It receives notifications and the questions agents ask you, never other task text or the screen."
          : "Keeps a light connection to each paired computer, so notifications still come after you swipe Øphio away or restart the phone, with nothing kept in the status bar. It receives notifications and the questions agents ask you, never other task text or the screen."}
    </p>
    {on && android && !android.running && (
      <Notice tone="warn" title="Android stopped notifications"
        action={<Button size="sm" disabled={pending} onClick={() => void watcher.configure({ enabled: true, detail })}>Start again</Button>}>
        Notifications for when Øphio is closed aren’t running.
      </Notice>
    )}
    {on && android && !android.unrestricted && (
      <Notice tone="warn" title="Notifications stop when Øphio closes"
        action={<Button size="sm" onClick={() => void watcher.battery()}>Allow</Button>}>
        Android lets Øphio wait for notifications in the background only when its battery use is Unrestricted.
      </Notice>
    )}
    {android?.running && computers.length > 0 && (
      <ul className="notif-computers" aria-label="Computers">
        {computers.map((p) => {
          const line = statusLine(state?.statuses[p.profileId]);
          return <li key={p.profileId} className={`notif-computers__row notif-computers__row--${line.tone}`}>
            <span className="notif-computers__dot" aria-hidden />
            <bdi className="notif-computers__name">{p.name}</bdi>
            <span className="notif-computers__state">{line.text}</span>
          </li>;
        })}
      </ul>
    )}
    {error && <p className="error-text" role="status">{error}</p>}
  </>;
}

/** This device's choice for each type: every type, under its heading, with
 * Alert, Silent or Off; the recommended one is marked. */
function WhatNotifies() {
  const { platform } = useApp();
  const releaseCheck = useHost((s) => s.capabilities?.features.includes(RELEASE_CHECK_FEATURE) ?? false);
  const agentUpdates = useHost((s) => s.capabilities?.features.includes("agent_update_policy_v1") ?? false);
  const store = deviceTiers(platform);
  useEffect(() => store.start(), [store]);
  const view = useStore(store, (s) => s, shallowEqual);
  const control = useCommandControl<Tiers>({
    kind: "instant",
    value: view.tiers,
    equal: shallowEqual,
    describe: (e) => (e instanceof Error && e.message ? e.message : "This phone didn’t take the change."),
    send: async (next) => {
      for (const t of ALERT_TYPES)
        if (next[t.type] !== store.get().tiers[t.type]) await store.setTier(t.type, next[t.type]);
    },
  });
  const tiers = control.value;
  const recommended = recommendedTiers();
  const shownTypes = ALERT_TYPES.filter((t) => (t.type !== "new_version" || releaseCheck) && (t.type !== "agent_update" || agentUpdates));
  const changed = shownTypes.filter((t) => tiers[t.type] !== t.tier).length;
  const set = (t: AlertTypeInfo, tier: Tier) => void control.set({ ...tiers, [t.type]: tier }, {
    label: `${t.name}: ${TIER_LABELS[tier]}`, what: `how ${t.name.toLowerCase()} notify you`, said: `${t.name}: ${TIER_LABELS[tier]}.`,
  });
  const openAndroid = view.native && platform.openNotificationSettings;
  return <Section title="What notifies you" id="settings-alert-types">
    <div className="notif-intro">
      <p className="settings-meta">
        {view.native
          ? "For this phone, from every computer it’s paired with."
          : "For this device, from every computer it’s paired with, while Øphio is open."}
        {" "}A dot marks the recommended choice.
      </p>
      <dl className="notif-legend">
        {TIERS.map((t) => <div key={t}><dt>{TIER_LABELS[t]}</dt><dd>{TIER_HINTS[t]}</dd></div>)}
      </dl>
    </div>
    {GROUPS.map((g) => (
      <section key={g.group} className="settings-card notif-group" aria-labelledby={`notif-group-${g.group}`}>
        <h3 id={`notif-group-${g.group}`} className="notif-group__title">{g.title}</h3>
        <p className="settings-meta notif-group__hint">{g.hint}</p>
        {shownTypes.filter((t) => t.group === g.group).map((t) => (
          <TypeRow key={t.type} info={t}>
            <Segmented<Tier> label={t.name} value={tiers[t.type]} onChange={(v) => set(t, v)}
              options={TIERS.map((v) => ({ value: v, label: TIER_LABELS[v], ...(recommended[t.type] === v ? { mark: "recommended" } : {}) }))} />
          </TypeRow>
        ))}
      </section>
    ))}
    <div className="notif-actions">
      <Button variant="secondary" disabled={!changed}
        onClick={() => void control.set({ ...tiers, ...Object.fromEntries(shownTypes.map((t) => [t.type, recommended[t.type]])) }, { label: "Use the recommended choices", what: "your notification choices", said: "Using the recommended choices.", done: "Using the recommended choices." })}>
        Use the recommended choices
      </Button>
      <p className="settings-meta" role="status">{!changed ? "These are the recommended choices." : `${changed === 1 ? "1 choice differs" : `${changed} choices differ`} from the recommended ones.`}</p>
    </div>
    {openAndroid && <div className="notif-android">
      <p className="settings-meta">Android keeps these as Øphio’s notification categories, so you can also change them, and their sounds, in Android’s settings.</p>
      <Button size="sm" variant="quiet" onClick={() => void platform.openNotificationSettings?.()}>Open Android settings</Button>
    </div>}
  </Section>;
}

function TypeRow({ info, note, children }: { info: AlertTypeInfo; note?: string; children: ReactNode }) {
  const id = useId();
  return <div className="notif-type" role="group" aria-labelledby={`${id}-n`} aria-describedby={`${id}-d`}>
    <p className="notif-type__name" id={`${id}-n`}>{info.name}</p>
    <p className="settings-meta notif-type__desc" id={`${id}-d`}>{info.description}{note && <> {note}</>}</p>
    {children}
  </div>;
}

/**
 * The computer's choice of which alerts reach the person, read while shown.
 * A choice shows on the tap and is saved behind it, against the revision it
 * was read at; one made meanwhile goes once that is answered.
 */
function useChoices() {
  const { session, profile } = useApp();
  const link = useLink();
  const store = notificationSettings(session);
  useEffect(() => store.follow(), [store]);
  const { view, error } = useStore(store, (s) => s, shallowEqual);
  const supported = useHost((s) => s.capabilities?.features.includes(NOTIFICATION_SETTINGS_FEATURE) ?? false);
  const typed = useHost((s) => s.capabilities?.features.includes(ALERT_TYPES_FEATURE) ?? false);
  const allowed = useHost((s) => !!(s.grants?.tasks || s.grants?.admin));
  const online = link.state === "online";
  // The revision this phone's last save made, until a read shows it.
  const saved = useRef(0);
  const control = useCommandControl<NotificationChoices>({
    kind: "instant",
    value: view ?? defaultNotificationSettings,
    equal: sameChoices,
    describe: (e) => describeError(e, profile.name),
    send: async (next, note) => {
      const revision = Math.max(store.get().view?.revision ?? 0, saved.current);
      try {
        await session.command({ command: "set_notification_settings", off: next.off, bundle: next.bundle, projects: next.projects.map((project) => ({ ...project, tiers: project.tiers?.filter((tier) => ALERT_TYPES.some((type) => type.type === tier.type && type.project)) })), types: true }, { label: note.label, expectedRevision: revision });
        saved.current = revision + 1;
      } finally { store.refresh(); }
    },
  });
  const save = (next: NotificationChoices, label: string, said: string, what: string) => control.set(next, { label, said, what });
  return {
    view, supported, typed, allowed, online, save, computer: profile.name,
    /** What to show: the person's latest choices over the computer's. */
    settings: { ...control.value, revision: view?.revision ?? 0 },
    ready: !!view && allowed && online,
    error: error ? describeError(error, profile.name) : null,
  };
}
type Choices = ReturnType<typeof useChoices>;

/** Why the choices can't be changed now, if they can't. */
function ChoicesNotes({ choices, what }: { choices: Choices; what: string }) {
  const { view, allowed, online, error } = choices;
  return <>
    {!allowed && <p className="settings-meta">This device needs the Run tasks or Manage access permission to change {what}.</p>}
    {!online && <Notice tone="unknown">Offline. Connect to change {what}.</Notice>}
    {online && !view && !error && <p className="settings-meta" role="status">Loading…</p>}
    {error && <Notice tone="unknown">{error}</Notice>}
  </>;
}

/** Each project's own choices, under the computer it belongs to. */
function Projects() {
  const entries = useComputerEntries();
  const several = entries.length > 1;
  const shown = entries.filter((e) => e.session);
  const away = entries.filter((e) => !e.session);
  return <Section title="Projects" id="settings-notification-projects">
    <p className="settings-meta notif-section-hint">
      A project can be muted, or choose its own for any kind. Its choices are kept on its computer, for every device paired with it.
    </p>
    {shown.map((e) => e.current
      ? <ComputerProjects key={e.profile.profileId} heading={several} />
      : <OnComputer key={e.profile.profileId} entry={e}><ComputerProjects heading /></OnComputer>)}
    {away.length > 0 && <p className="settings-meta">{away.map((e: ComputerEntry) => e.profile.name).join(", ")}: open {away.length === 1 ? "it" : "them"} from the computer menu to see {away.length === 1 ? "its" : "their"} projects here.</p>}
  </Section>;
}

function ComputerProjects({ heading }: { heading: boolean }) {
  const { ui } = useApp();
  const choices = useChoices();
  const { settings, supported, typed, computer, view } = choices;
  const projects = useHost((s) => s.projects);
  const listed = Object.values(projects).filter((p) => !p.one_off).sort((a, b) => a.name.localeCompare(b.name));
  const id = useId();
  return <div className="notif-computer" role="group" aria-labelledby={heading ? `${id}-h` : undefined}>
    {heading && <h3 className="notif-computer__name" id={`${id}-h`}>On <bdi>{computer}</bdi></h3>}
    {!supported ? <Notice>Update Øphio on {computer} to choose what notifies you from each of its projects.</Notice>
      : !listed.length ? <p className="settings-meta">{computer} has no projects yet.</p>
      : <>
        {!view && <ChoicesNotes choices={choices} what="these" />}
        <div className="group">
          {listed.map((p) => <ListRow key={p.project_id} title={p.name} subtitle={projectSummary(settings, p.project_id, typed)} chevron
            onClick={() => ui.push({ screen: "settings", section: "settings-notifications", projectId: p.project_id })} />)}
        </div>
      </>}
  </div>;
}

/** Notification detail, on this phone, and one notification per project,
 * on the computer. */
function HowTheyLook() {
  const { platform } = useApp();
  const detail = useStore(notificationDetail, (s) => s);
  const watcher = backgroundAlerts(platform);
  const choices = useChoices();
  const { settings, supported, ready, save, computer } = choices;
  const id = useId();
  const changeDetail = (value: NotificationDetail) => {
    setNotificationDetail(value);
    // The watcher writes its own notifications, so it keeps the same choice.
    const kept = watcher?.get().state?.settings;
    if (watcher && kept) void watcher.configure({ ...kept, detail: value });
  };
  if (!platform.notify && !supported) return null;
  return <Section title="How they look" id="settings-alert-look">
    <div className="settings-card settings-stack">
      {platform.notify && <div className="field">
        <label htmlFor={`${id}-detail`} className="field__label">Notification detail</label>
        <select
          id={`${id}-detail`}
          className="field__input"
          aria-describedby={`${id}-detail-h`}
          value={detail}
          onChange={(e) => changeDetail(e.target.value === "task_title" ? "task_title" : "minimal")}
        >
          <option value="minimal">Minimal</option>
          <option value="task_title">Include task title</option>
        </select>
        <p className="field__hint" id={`${id}-detail-h`}>
          {detail === "task_title"
            ? "Adds the task title. Titles can contain private details, and they appear on your lock screen."
            : "Shows the computer and project, not the task’s title."}
        </p>
      </div>}
      {supported && <div className="settings-topic">
        <label className="settings-check">
          <input type="checkbox" checked={settings.bundle} disabled={!ready} aria-describedby={`${id}-bundle`}
            onChange={(e) => save({ ...settings, bundle: e.target.checked },
              e.target.checked ? "One notification per project" : "A notification for each alert",
              e.target.checked ? "One notification per project." : "A notification for each alert.", "one notification per project")} />
          One notification per project
        </label>
        <p className="settings-meta" id={`${id}-bundle`}>
          On {computer}: once a project has a second notification, they become one that counts them, such as “Website has 3 notifications”. Tapping it opens the project’s Needs you.
        </p>
        <OldDefaults choices={choices} />
      </div>}
    </div>
  </Section>;
}

/** An older computer's "every project" switches, which hold notifications
 * back on every phone: said, with a way to turn them back on. */
function OldDefaults({ choices }: { choices: Choices }) {
  const { settings, typed, ready, save, computer } = choices;
  if (typed || !settings.off.length) return null;
  const names = TOPICS.filter((t) => settings.off.includes(t.topic)).map((t) => t.label.toLowerCase());
  return <Notice title={`${computer} holds some back`}
    action={<Button size="sm" disabled={!ready} onClick={() => void save({ ...settings, off: [] as AlertTopic[] }, "Turn every kind back on", "Every kind notifies again.", "which notifications it holds back")}>Turn them back on</Button>}>
    It doesn’t send {names.join(", ")} to any phone. Update Øphio on {computer} to choose these per phone instead.
  </Notice>;
}

function Elsewhere() {
  const { ui } = useApp();
  return <Section title="Elsewhere" id="settings-alert-elsewhere">
    <p className="settings-meta notif-section-hint">
      Messages from Øphio in Telegram or Discord aren’t covered here. Those apps notify you by their own settings.
    </p>
    <div className="group">
      <ListRow title="Chat links" subtitle="Which Telegram and Discord chats Øphio talks to, in Connections" chevron
        onClick={() => ui.up({ screen: "connections" })} />
    </div>
  </Section>;
}

/** One project's notifications over each device's own. */
export function ProjectNotificationsPage({ projectId }: { projectId: string }) {
  const typed = useHost((s) => s.capabilities?.features.includes(ALERT_TYPES_FEATURE) ?? false);
  return typed ? <ProjectTypesPage projectId={projectId} /> : <ProjectTopicsPage projectId={projectId} />;
}

type Own = Tier | "default";

/** A computer that keeps a choice per type: muted, each type's own, and
 * one notification or each. */
function ProjectTypesPage({ projectId }: { projectId: string }) {
  const { platform } = useApp();
  const choices = useChoices();
  const { settings, supported, ready, save, computer } = choices;
  const project = useHost((s) => s.projects[projectId]);
  const store = deviceTiers(platform);
  useEffect(() => store.start(), [store]);
  const device = useStore(store, (s) => s.tiers, shallowEqual);
  const id = useId();
  const kept = projectChoices(settings, projectId);
  const mine: ProjectNotifications = kept ?? { project_id: projectId, mute: false, on: [], off: [] };
  const name = project?.name ?? "This project";
  const change = (next: ProjectNotifications, label: string, said: string, what: string) =>
    save({ ...settings, projects: withProject(settings, next) }, label, said, what);
  const setType = (t: AlertTypeInfo, value: Own) => change(withOwnTier(mine, t.type, value === "default" ? null : value),
    `${t.name} in ${name}`, `${t.name}: ${value === "default" ? "default" : TIER_LABELS[value]} in ${name}.`, `how ${t.name.toLowerCase()} notify you from ${name}`);
  const off = !ready || mine.mute;
  const options = (type: AlertType): SegmentOption<Own>[] => [
    // The computer's own choice for every project, brought over from an
    // older switch, comes before this phone's.
    { value: "default", label: `Default (${TIER_LABELS[settings.tiers?.find((t) => t.type === type)?.tier ?? device[type]]})`, disabled: off },
    ...TIERS.map((v) => ({ value: v, label: TIER_LABELS[v], disabled: off })),
  ];
  const bundle: "default" | "on" | "off" = mine.bundle === undefined ? "default" : mine.bundle ? "on" : "off";
  const setBundle = (value: "default" | "on" | "off") => {
    const { bundle: _, ...rest } = mine;
    change(value === "default" ? rest : { ...rest, bundle: value === "on" }, `Notifications for ${name}`,
      value === "default" ? `${name} uses the default.` : value === "on" ? `One notification for ${name}.` : `A notification for each in ${name}.`,
      `how ${name}’s notifications show`);
  };
  const changes = (mine.tiers?.length ?? 0) + (mine.bundle !== undefined ? 1 : 0);
  return <Screen title="Notifications" subtitle={name} className="settings-screen"><div className="settings-stack">
    {!supported ? <Notice>Update Øphio on {computer} to choose what notifies you from each project.</Notice> : <>
      <ChoicesNotes choices={choices} what="these" />
      <Section title="This project" id="settings-project-notifications">
        <div className="settings-card settings-stack">
          <label className="settings-check">
            <input type="checkbox" checked={mine.mute} disabled={!ready} aria-describedby={`${id}-mute`}
              onChange={(e) => change({ ...mine, mute: e.target.checked }, e.target.checked ? `Mute ${name}` : `Unmute ${name}`,
                e.target.checked ? `${name} is muted.` : `${name} notifies you again.`, `whether ${name} is muted`)} />
            Mute this project
          </label>
          <p className="settings-meta" id={`${id}-mute`}>
            {mine.mute
              ? "Nothing from it notifies you, on any device. What needs you still waits in its Needs you, and a task you asked to hear about still tells you when it’s done."
              : `Its choices below are kept on ${computer} and apply on every device paired with it. Default follows each device’s own choice; on this phone that’s the one in brackets.`}
          </p>
        </div>
      </Section>
      <Section title="What notifies you" id="settings-project-types">
      {GROUPS.map((g) => {
        const types = ALERT_TYPES.filter((t) => t.group === g.group && t.project);
        return <section key={g.group} className={`settings-card notif-group${mine.mute ? " notif-group--muted" : ""}`} aria-labelledby={`${id}-${g.group}`}>
          <h3 id={`${id}-${g.group}`} className="notif-group__title">{g.title}</h3>
          {types.map((t) => {
            const own = ownTier(mine, t.type);
            return <TypeRow key={t.type} info={t}>
              <Segmented<Own> label={t.name} value={own ?? "default"} onChange={(v) => setType(t, v)} options={options(t.type)} />
            </TypeRow>;
          })}
        </section>;
      })}
      </Section>
      <Section title="How they look" id="settings-project-look">
        <div className="settings-card settings-stack">
          <p className="settings-meta">One notification for this project</p>
          <Segmented<"default" | "on" | "off"> label={`One notification for ${name}`} value={bundle} onChange={setBundle} options={[
            { value: "default", label: `Default (${settings.bundle ? "one" : "separate"})`, disabled: !ready },
            { value: "on", label: "One", disabled: !ready },
            { value: "off", label: "Separate", disabled: !ready },
          ]} />
        </div>
      </Section>
      {kept && <div className="notif-actions">
        <Button variant="secondary" disabled={!ready}
          onClick={() => change({ project_id: projectId, mute: false, on: [], off: [] }, `Reset notifications for ${name}`, `${name} uses the defaults.`, `${name}’s notifications`)}>
          Reset to the defaults
        </Button>
        {!mine.mute && changes > 0 && <p className="settings-meta" role="status">{changes === 1 ? "1 change" : `${changes} changes`} from the defaults.</p>}
      </div>}
    </>}
  </div></Screen>;
}

type Tri = "default" | "on" | "off";

/** An older computer's choices, per topic: muted, each topic, and folding. */
function ProjectTopicsPage({ projectId }: { projectId: string }) {
  const choices = useChoices();
  const { settings, supported, ready, save, computer } = choices;
  const project = useHost((s) => s.projects[projectId]);
  const id = useId();
  const mine: ProjectNotifications = projectChoices(settings, projectId) ?? { project_id: projectId, mute: false, on: [], off: [] };
  const name = project?.name ?? "This project";
  const change = (next: ProjectNotifications, label: string, said: string, what: string) =>
    save({ ...settings, projects: withProject(settings, next) }, label, said, what);
  const topicChoice = (topic: AlertTopic): Tri => mine.off.includes(topic) ? "off" : mine.on.includes(topic) ? "on" : "default";
  const setTopic = (topic: AlertTopic, label: string, value: Tri) => change({
    ...mine,
    on: value === "on" ? [...mine.on.filter((t) => t !== topic), topic] : mine.on.filter((t) => t !== topic),
    off: value === "off" ? [...mine.off.filter((t) => t !== topic), topic] : mine.off.filter((t) => t !== topic),
  }, `${label} in ${name}`, `${label}: ${value === "default" ? "default" : value} in ${name}.`, `${label.toLowerCase()} alerts in ${name}`);
  const setBundle = (value: Tri) => {
    const { bundle: _, ...rest } = mine;
    change(value === "default" ? rest : { ...rest, bundle: value === "on" }, `Notifications for ${name}`,
      value === "default" ? `${name} uses the default.` : value === "on" ? `One notification for ${name}.` : `A notification for each alert in ${name}.`,
      `how ${name}’s alerts show`);
  };
  const off = !ready || mine.mute;
  const tri = (onDefault: boolean, on: string, offLabel: string) => [
    { value: "default" as const, label: `Default (${(onDefault ? on : offLabel).toLowerCase()})`, disabled: off },
    { value: "on" as const, label: on, disabled: off },
    { value: "off" as const, label: offLabel, disabled: off },
  ];
  return <Screen title="Notifications" subtitle={name} className="settings-screen"><div className="settings-stack">
    {!supported ? <Notice>Update Øphio on {computer} to choose what alerts you from each project.</Notice> : <>
      <ChoicesNotes choices={choices} what="these" />
      <Section title="This project" id="settings-project-notifications">
        <div className="settings-card settings-stack">
          <label className="settings-check">
            <input type="checkbox" checked={mine.mute} disabled={!ready} aria-describedby={`${id}-mute`}
              onChange={(e) => change({ ...mine, mute: e.target.checked }, e.target.checked ? `Mute ${name}` : `Unmute ${name}`,
                e.target.checked ? `${name} is muted.` : `${name} alerts again.`, `whether ${name} is muted`)} />
            Mute this project
          </label>
          <p className="settings-meta" id={`${id}-mute`}>No alerts from it at all. What needs you still waits in its Needs you.</p>
        </div>
        <div className="settings-card settings-stack">
          <p className="settings-meta">One notification for this project</p>
          <Segmented<Tri> label={`One notification for ${name}`} value={mine.bundle === undefined ? "default" : mine.bundle ? "on" : "off"}
            onChange={setBundle} options={tri(settings.bundle, "One", "Separate")} />
        </div>
      </Section>
      <Section title="What alerts you" id="settings-project-topics">
        <div className="settings-card settings-stack">
          <p className="settings-meta">Update Øphio on {computer} to choose Alert, Silent or Off for each kind in this project.</p>
          {TOPICS.filter(t => t.topic !== "reports").map((t) => <div key={t.topic} className="settings-topic">
            <p>{t.label}</p>
            <Segmented<Tri> label={t.label} value={topicChoice(t.topic)} onChange={(v) => setTopic(t.topic, t.label, v)}
              options={tri(!settings.off.includes(t.topic), "On", "Off")} />
          </div>)}
        </div>
      </Section>
      {projectChoices(settings, projectId) && <Button variant="quiet" disabled={!ready}
        onClick={() => change({ project_id: projectId, mute: false, on: [], off: [] }, `Reset notifications for ${name}`, `${name} uses the defaults.`, `${name}’s alerts`)}>
        Reset to the defaults
      </Button>}
    </>}
  </div></Screen>;
}

