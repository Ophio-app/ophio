// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState, type ReactNode } from "react";
import { YourReports } from "../../reports/YourReports";
import { openReport } from "../../reports/request";
import { Screen } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { AgentsAndAccounts } from "./AgentsAndAccounts";
import { DataAndVoice } from "./DataAndVoice";
import { Notifications, ProjectNotificationsPage } from "./Notifications";
import { Versions } from "./Versions";
import { AppUpdate, useAppOffer } from "./AppUpdate";
import { LaunchDefaults } from "./LaunchDefaults";
import { FileDelivery } from "./FileDelivery";
import { Appearance } from "./Appearance";
import { Warnings } from "./Warnings";
import { Lock } from "./Lock";
import { ListRow, Section } from "../../design/ListRow";
import {
  Bell, Bot, Bug, Flag, FolderDown, GitPullRequest, Globe, KeyRound, Link2, Lock as LockIcon, Mic, MonitorSmartphone, Package, Palette, TriangleAlert,
  type LucideIcon,
} from "lucide-react";
import { useGithubSupported } from "../../app/github";
import { TextField } from "../../design/Field";
import { Button } from "../../design/Button";
import { acceptObjectLink, objectLinkProblem } from "../../platform/links";
import { useApp, useLink } from "../../app/context";
import "./settings.css";

interface Page {
  /** The section the page opens with; the page's other sections are found by their own. */
  id: string;
  title: string;
  subtitle: string;
  icon: LucideIcon;
  /** Other sections on the same page. */
  also?: string[];
  /** Its one section's heading repeats the page's title. */
  one?: boolean;
  /** The Settings section it's listed under. */
  group: Group;
  body: () => ReactNode;
}
type Group = "agents" | "computer" | "device";

/** The app's own settings, a page each, in the order Settings lists them. */
const PAGES: readonly Page[] = [
  { id: "default-agent", title: "Default agent and permissions", subtitle: "Which agent new tasks start with, and whether agents ask first", icon: Flag, also: ["launch-flags"], group: "agents", body: () => <LaunchDefaults /> },
  { id: "settings-agents", title: "Agent apps", subtitle: "Claude Code, Codex and others: sign-in, updates and questions", icon: Bot, one: true, group: "agents", body: () => <AgentsAndAccounts /> },
  { id: "settings-files", title: "Where files go", subtitle: "Files from your checklists: this phone, Taildrop, a folder or a drive", icon: FolderDown, also: ["settings-files-folders", "settings-files-drives"], group: "computer", body: () => <FileDelivery /> },
  { id: "settings-link", title: "Open a link from chat", subtitle: "A task, file or approval someone copied from a chat message", icon: Link2, one: true, group: "computer", body: () => <ItemLink /> },
  { id: "settings-notifications", title: "Notifications", subtitle: "What alerts you, for every project and each one", icon: Bell, group: "device", body: () => <Notifications /> },
  { id: "settings-appearance", title: "Appearance", subtitle: "Light or dark, and motion", icon: Palette, one: true, group: "device", body: () => <Appearance /> },
  { id: "settings-lock", title: "Lock", subtitle: "Biometrics or screen lock to open the app", icon: LockIcon, one: true, group: "device", body: () => <Lock /> },
  { id: "settings-warnings", title: "Warnings", subtitle: "Asking before actions, and the warnings you turned off", icon: TriangleAlert, one: true, group: "device", body: () => <Warnings /> },
  { id: "settings-voice", title: "Data and voice", subtitle: "Speech, voice control, and the drafts this device keeps", icon: Mic, also: ["settings-voice-control", "settings-data"], group: "device", body: () => <DataAndVoice /> },
  { id: "settings-reports", title: "Your reports", subtitle: "Progress, questions and what you shared", icon: Bug, one: true, group: "device", body: () => <YourReports /> },
  { id: "settings-versions", title: "Versions", subtitle: "This app and Øphio on the computer", icon: Package, one: true, group: "device", body: () => <Versions /> },
];
const pageOf = (section: string | undefined) => section ? PAGES.find((p) => p.id === section || p.also?.includes(section)) : undefined;

export default function Settings({ route }: RouteProps<"settings">) {
  const { session, ui } = useApp(), connection = useLink();
  useEffect(() => {
    if (connection.state !== "online") return;
    let alive = true;
    void session.query({ kind: "readiness" }).then((result) => {
      if (alive) session.state.set((s) => ({ ...s, readiness: result.report }));
    }).catch(() => undefined);
    return () => { alive = false; };
  }, [session, connection.state]);
  const github = useGithubSupported();
  const page = pageOf(route.section);
  // Only the list checks here; Versions checks for itself.
  const update = useAppOffer(!page);
  useEffect(() => {
    // A link to a section further down its page opens there.
    if (page && route.section !== page.id) document.getElementById(route.section ?? "")?.scrollIntoView();
  }, [page, route.section]);
  if (page?.id === "settings-notifications" && route.projectId)
    return <ProjectNotificationsPage projectId={route.projectId} />;
  if (page?.id === "settings-reports") return <Screen title="Your reports" className="settings-screen"><YourReports reportId={route.reportId} /></Screen>;
  if (page)
    return <Screen title={page.title} className={`settings-screen${page.one ? " settings-page--one" : ""}`}>
      <div className="settings-stack">{page.body()}</div>
    </Screen>;
  const pages = (group: Group) => PAGES.filter((p) => p.group === group).map((p) =>
    <ListRow key={p.id} icon={p.icon} title={p.title} subtitle={p.subtitle} chevron onClick={() => ui.push({ screen: "settings", section: p.id })} />);
  return <Screen title="Settings" className="settings-screen"><div className="settings-stack">
    {update?.offer && <Section title="Update available" id="settings-update"><div className="settings-card settings-card--update settings-stack">
      <AppUpdate found={update} />
    </div></Section>}
    <Section title="Agents" id="settings-agents-group"><div className="group">
      {pages("agents")}
      <ListRow icon={KeyRound} title="Accounts" subtitle="Sign-ins and keys the computer gives its tasks" chevron
        onClick={() => ui.push({ screen: "accounts" })} />
      <ListRow icon={Globe} title="Agent logins" subtitle="Usernames and passwords agents use on websites" chevron
        onClick={() => ui.push({ screen: "agent_logins" })} />
    </div></Section>
    <Section title="This computer" id="settings-computer"><div className="group">
      <ListRow icon={MonitorSmartphone} title="Connections and readiness" subtitle="Paired devices and what each may do, chat links, and what’s ready to use" chevron
        onClick={() => ui.up({ screen: "connections" })} />
      {github && <ListRow icon={GitPullRequest} title="GitHub" subtitle="Each project’s pull requests, merging and issue finding" chevron
        onClick={() => ui.push({ screen: "github" })} />}
      {pages("computer")}
    </div></Section>
    <Section title="This device" id="settings-app"><div className="group">
      <ListRow icon={Bug} title="Report a problem" subtitle="Add a sentence, if you like" chevron onClick={() => openReport()} />
      {pages("device")}
    </div></Section>
  </div></Screen>;
}

function ItemLink() {
  const [reference, setReference] = useState("");
  const [referenceError, setReferenceError] = useState<string | null>(null);
  const checkReference = () => {
    const value = reference.trim(), problem = objectLinkProblem(value);
    setReferenceError(problem);
    if (problem) return;
    acceptObjectLink(value); setReference("");
  };
  return <Section title="Open a link from chat" id="settings-link"><div className="settings-card settings-stack">
    <p className="muted">Paste a reference copied from a Telegram or Discord message. The computer checks this device may see it before you choose to open it.</p>
    <TextField label="Item link" value={reference} onChange={(next) => { setReference(next); setReferenceError(null); }} error={referenceError} autoComplete="off" />
    <Button disabled={!reference.trim()} onClick={checkReference}>Check link</Button>
  </div></Section>;
}
