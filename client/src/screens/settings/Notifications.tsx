// SPDX-License-Identifier: Apache-2.0
import { useEffect, useId, useRef, useState } from "react";
import { useCommandControl } from "../../app/commandControl";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { Segmented } from "../../design/Chip";
import { ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { shallowEqual, useStore } from "../../state/store";
import type { AlertStatus } from "../../host/link";
import type { AlertTopic, NotificationSettings, ProjectNotifications } from "../../protocol/types";
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
  notificationDetail,
  setNotificationDetail,
  type NotificationDetail,
} from "../../platform/notifications";
import { backgroundAlerts, type Alerts } from "../../platform/alerts";
import { AlertPermissionAction, useNotificationAccess } from "../../shell/AlertsOffer";

export function Notifications() {
  const { platform } = useApp();
  const detail = useStore(notificationDetail, (s) => s);
  const { access, permission, error: permissionError } = useNotificationAccess();
  const watcher = backgroundAlerts(platform);
  const id = useId();
  const changeDetail = (value: NotificationDetail) => {
    setNotificationDetail(value);
    // The watcher writes its own alerts, so it keeps the same choice.
    const settings = watcher?.get().state?.settings;
    if (watcher && settings) void watcher.configure({ ...settings, detail: value });
  };
  return (<>
    <Section title="On this phone" id="settings-notifications">
      <div className="settings-card settings-stack">
        {platform.notificationPermission && <Notice
          tone={permission === "blocked" ? "warn" : permissionError ? "unknown" : "info"}
          title={permission === "granted" ? "Alerts are on" : permission === "promptable" ? "Alerts are off" : permission === "blocked" ? "Alerts are blocked in Android settings" : permissionError ? "Task alert permission is unknown" : "Checking task alerts…"}
          action={!permission && permissionError ? <Button onClick={() => void access.refresh()}>Check alerts again</Button> : <AlertPermissionAction />}>
          {permissionError}
        </Notice>}
        <p>
          Øphio alerts you when an agent asks something, a task needs your
          approval or an answer, or a task finishes.
        </p>
        {platform.notify ? (
          <div className="field">
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
          </div>
        ) : (
          <Notice>
            System notifications aren’t available in this build. Needs you stays
            available inside the app.
          </Notice>
        )}
        {watcher ? <ClosedAlerts watcher={watcher} detail={detail} /> : (
          <p className="settings-meta">In this build, alerts arrive only while the app is open.</p>
        )}
      </div>
    </Section>
    <AlertChoices />
  </>);
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
        await session.command({ command: "set_notification_settings", off: next.off, bundle: next.bundle, projects: next.projects }, { label: note.label, expectedRevision: revision });
        saved.current = revision + 1;
      } finally { store.refresh(); }
    },
  });
  const save = (next: NotificationChoices, label: string, said: string, what: string) => control.set(next, { label, said, what });
  return {
    view, supported, allowed, online, save, computer: profile.name,
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

/** What alerts in every project, folding them per project, and each project's own. */
function AlertChoices() {
  const reportsSupported = useHost(s => s.capabilities?.features.includes("reports_v1") ?? false);
  const { ui } = useApp();
  const choices = useChoices();
  const { settings, supported, ready, save, computer } = choices;
  const projects = useHost((s) => s.projects);
  const id = useId();
  if (!supported) return <Section title="What alerts you" id="settings-alert-topics">
    <Notice>Update Øphio on {computer} to choose what alerts you, for every project and each one.</Notice>
  </Section>;
  const turn = (topic: AlertTopic, on: boolean, label: string) =>
    save({ ...settings, off: on ? settings.off.filter((t) => t !== topic) : [...settings.off, topic] },
      `${on ? "Turn on" : "Turn off"} ${label.toLowerCase()} alerts`, `${label}: ${on ? "on" : "off"} for every project.`, `${label.toLowerCase()} alerts`);
  const listed = Object.values(projects).filter((p) => !p.one_off).sort((a, b) => a.name.localeCompare(b.name));
  return <>
    <Section title="What alerts you" id="settings-alert-topics">
      <div className="settings-card settings-stack">
        <p className="settings-meta">The default for every project on {computer}, on every phone paired with it. Each project can choose its own below.</p>
        <ChoicesNotes choices={choices} what="these" />
        {TOPICS.filter(t => t.topic !== "reports" || reportsSupported).map((t) => <div key={t.topic} className="settings-topic">
          <label className="settings-check">
            <input type="checkbox" checked={!settings.off.includes(t.topic)} disabled={!ready} aria-describedby={`${id}-${t.topic}`}
              onChange={(e) => turn(t.topic, e.target.checked, t.label)} />
            {t.label}
          </label>
          <p className="settings-meta" id={`${id}-${t.topic}`}>{t.hint}</p>
        </div>)}
      </div>
      <div className="settings-card settings-stack">
        <label className="settings-check">
          <input type="checkbox" checked={settings.bundle} disabled={!ready} aria-describedby={`${id}-bundle`}
            onChange={(e) => save({ ...settings, bundle: e.target.checked },
              e.target.checked ? "One notification per project" : "A notification for each alert",
              e.target.checked ? "One notification per project." : "A notification for each alert.", "one notification per project")} />
          One notification per project
        </label>
        <p className="settings-meta" id={`${id}-bundle`}>
          Once a project has a second alert, they become one that counts them, such as “Website has 3 notifications”. Tapping it opens the project’s Needs you. It alerts again only for something that needs you.
        </p>
      </div>
    </Section>
    {listed.length > 0 && <Section title="Projects" id="settings-notification-projects">
      <div className="group">
        {listed.map((p) => <ListRow key={p.project_id} title={p.name} subtitle={summary(settings, p.project_id)} chevron
          onClick={() => ui.push({ screen: "settings", section: "settings-notifications", projectId: p.project_id })} />)}
      </div>
    </Section>}
  </>;
}

/** One project's choices, said briefly. */
function summary(settings: NotificationSettings, projectId: string): string {
  const p = projectChoices(settings, projectId);
  if (!p) return "Default";
  if (p.mute) return "Muted";
  const parts = [
    ...(p.on.length ? [`${p.on.length} on`] : []),
    ...(p.off.length ? [`${p.off.length} off`] : []),
    ...(p.bundle === true ? ["one notification"] : p.bundle === false ? ["separate notifications"] : []),
  ];
  return parts.join(", ").replace(/^./, (c) => c.toUpperCase());
}

type Tri = "default" | "on" | "off";

/** One project's alerts over every project's: muted, each topic, and folding. */
export function ProjectNotificationsPage({ projectId }: { projectId: string }) {
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

/** How the watcher's own connection to the computer stands. The app's
 * connection, in the header, is a separate one: while it is live, a watcher
 * still connecting or out of reach is said as the watcher's, so the two
 * never seem to disagree. */
function statusText(status: AlertStatus | undefined, name: string, live: boolean): string {
  switch (status) {
    case "watching": return `Ready for alerts from ${name}.`;
    case "update_needed": return `Update Øphio on ${name} to get alerts while the app is closed.`;
    case "unreachable": return live
      ? `Alerts for when the app is closed can’t reach ${name} yet and keep trying. Until then they arrive while the app is open.`
      : `Can’t reach ${name} right now. Alerts that came up meanwhile arrive once it’s back.`;
    case "not_allowed": return `${name} no longer accepts this phone. Pair again to get its alerts.`;
    default: return live
      ? `Alerts for when the app is closed are still starting. Until then, alerts from ${name} arrive while the app is open.`
      : `Connecting to ${name}…`;
  }
}

/** One switch for every paired computer: a watcher that runs without the app's screen. */
function ClosedAlerts({ watcher, detail }: { watcher: Alerts; detail: NotificationDetail }) {
  const { profile } = useApp();
  const { state, pending, error } = useStore(watcher, (s) => s, shallowEqual);
  const live = useLink().state === "online";
  useEffect(() => watcher.follow(), [watcher]);
  // The switch shows the choice on the tap; Android's answer follows.
  const [asked, setAsked] = useState<boolean | null>(null);
  const on = asked ?? !!state?.settings?.enabled;
  const android = state?.android;
  const turn = (enabled: boolean) => {
    setAsked(enabled);
    void watcher.configure({ enabled, detail }).finally(() => setAsked((a) => (a === enabled ? null : a)));
  };
  return <>
    <label className="settings-check">
      <input type="checkbox" checked={on} disabled={!state}
        onChange={(e) => turn(e.target.checked)} />
      Alerts when the app is closed
    </label>
    <p className="settings-meta">
      {!on
        ? "Off: alerts arrive only while the app is open."
        : android && !android.unrestricted
          ? "Keeps a light connection to each paired computer while the app is open. It receives alerts and the questions agents ask you, never other task text or the screen."
          : "Keeps a light connection to each paired computer, so alerts arrive after you swipe the app away or restart the phone, with nothing kept in the status bar. It receives alerts and the questions agents ask you, so you can answer from the notification, never other task text or the screen."}
    </p>
    {on && android && (android.running
      ? <p className="settings-meta" role="status">{statusText(state.statuses[profile.profileId], profile.name, live)}</p>
      : <Notice tone="warn" title="Android stopped alerts"
          action={<Button size="sm" disabled={pending} onClick={() => void watcher.configure({ enabled: true, detail })}>Start again</Button>}>
          Alerts for when the app is closed aren’t running.
        </Notice>)}
    {on && android && !android.unrestricted && (
      <Notice tone="warn" title="Alerts stop when the app closes"
        action={<Button size="sm" onClick={() => void watcher.battery()}>Allow</Button>}>
        Android lets Øphio wait for alerts in the background only when its battery use is Unrestricted.
      </Notice>
    )}
    {android && (
      <dl className="settings-facts">
        <div><dt>Needs you: questions, approvals and blocked tasks</dt><dd>{android.needsYou ? "On" : "Off"} <Button size="sm" variant="quiet" onClick={() => void watcher.channel("needs-you")}>Change</Button></dd></div>
        <div><dt>Finished tasks, and tasks that stopped with a problem</dt><dd>{android.finished ? "On" : "Off"} <Button size="sm" variant="quiet" onClick={() => void watcher.channel("finished")}>Change</Button></dd></div>
      </dl>
    )}
    {error && <p className="error-text" role="status">{error}</p>}
  </>;
}
