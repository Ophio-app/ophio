// SPDX-License-Identifier: Apache-2.0
//
// A project's GitHub, from Work and Talk, as two doors. Issues lists the
// open issues: each starts with the one agent that works it, in a tap, and
// New issue files one. Find & fix is where reviews start, one agent reviewing
// the project and filing issues, up to a limit, for another to fix, and
// where the open issues are sorted. Merging by itself is one switch that
// warns before it turns on; everything else is behind the gear. Without a
// project, this is Settings › GitHub: the Git projects, each opening its
// GitHub settings.

import { useEffect, useId, useRef, useState } from "react";
import {
  Bot, CircleDot, ExternalLink, FileSearch, FileText, FolderGit2, KeyRound, ListChecks, Plus, Search, Settings, Wrench,
} from "lucide-react";
import { describeError, shallowEqual, useApp, useHost, useLink } from "../../app/context";
import {
  GROUP_WORDS, expectItem, groupItems, linkText, rememberRole, reminderText, useGithubIssuesSupported, useGithubItems,
  useGithubProject, useLiveItems, warningText,
} from "../../app/github";
import { FINDINGS_FILE, useFindingsSupported } from "../../app/findings";
import { GITHUB_CLOSE_UPDATE, GITHUB_ISSUES_UPDATE, GITHUB_RUN_FEATURE } from "../../app/githubFeature";
import { CLOSES, FOLDED, useNeed } from "../../app/githubNeed";
import { LAUNCH_POLICY_FEATURE } from "../../app/launchPolicy";
import { LaunchLine } from "../talk/LaunchLine";
import type { AgentChoice } from "../../app/ui";
import { useWarning } from "../../app/warnings";
import { PRODUCT_NAME } from "../../brand";
import { Button, IconButton } from "../../design/Button";
import { Segmented } from "../../design/Chip";
import { IssuesOrPrs } from "./GithubDoors";
import { TextArea } from "../../design/Field";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import { commandResult } from "../../host/commandResult";
import type {
  GithubItem, GithubLink, GithubProjectView, Project, Task, TaskMode, TaskPlacement, TriageGroup,
} from "../../protocol/types";
import { going } from "../../state/githubStatus";
import { useMay } from "../../shell/NotAllowed";
import { Screen } from "../../shell/Screen";
import { projectTasks } from "../../state/projection";
import { AgentSheet } from "../talk/AgentSheet";
import { agentLabel, createTaskCommand, writerHold, writerHoldText } from "../talk/intent";
import { TaskRow, useOpenTask } from "../work/shared";
import { pendingQueueItems, useQueue } from "../work/useQueue";
import type { RouteProps } from "../types";
import {
  Gate, type GithubSettingsControl, ItemRow, MergeWarnings, type Run, ThroughMergeWarning, TurnOn, agentBlocker, agentSent, answerWarning, asAgent, asLaunch,
  nextUp, startOrQueue, useDraftGithub, useGithubRun, useGithubSettings, useRole, useStartTask, withDirection,
} from "./common";
import { Changing } from "../../design/StateLine";
import { EXPLAIN, GithubSheet, SettingsSheet } from "./GithubSettings";
import { NeedCard, useCloseIssue } from "./IssueNeed";
import { RunAll } from "./RunAll";
import { ReviewerPick, useReviewer } from "./Reviewer";
import "../settings/settings.css";


/** Git projects, by name; one-offs aren't kept long enough to link. */
export function gitProjects(projects: Project[]): Project[] {
  return projects.filter((p) => p.vcs?.kind === "git" && !p.one_off).sort((a, b) => a.name.localeCompare(b.name));
}

export default function Github({ route }: RouteProps<"github">) {
  const { profile } = useApp();
  const project = useHost((s) => (route.projectId ? s.projects[route.projectId] ?? null : null));
  if (project) return <ProjectGithub project={project} shown={route.view === "find_fix" ? "find_fix" : "issues"} />;
  return (
    <Screen title="GitHub" subtitle={profile.name} className="settings-screen github-screen">
      <div className="settings-stack">
        <Gate what="see GitHub settings">
          {route.projectId ? <Notice tone="unknown">This project isn’t on {profile.name} any more.</Notice> : <ProjectList />}
        </Gate>
      </div>
    </Screen>
  );
}

/** Settings › GitHub: each project opens its GitHub settings, not its issues. */
function ProjectList() {
  const projects = useHost((s) => gitProjects(Object.values(s.projects)), shallowEqual);
  const [open, setOpen] = useState<Project | null>(null);
  return <>
    <p className="settings-meta">{EXPLAIN}</p>
    {projects.length ? (
      <Section title="Projects">
        <div className="group">
          {projects.map((p) => (
            <ListRow key={p.project_id} icon={FolderGit2} title={p.name} subtitle={p.host_path} chevron
              ariaLabel={`GitHub settings for ${p.name}`} onClick={() => setOpen(p)} />
          ))}
        </div>
      </Section>
    ) : <Empty icon={FolderGit2} title="No Git projects">Add a project whose folder is a Git repository with its `origin` on GitHub.</Empty>}
    {open && <GithubSheet project={open} open onClose={() => setOpen(null)} />}
  </>;
}

type Shown = "issues" | "find_fix";

function ProjectGithub({ project, shown }: { project: Project; shown: Shown }) {
  const { profile, ui } = useApp();
  const online = useLink().state === "online";
  const admin = useMay("admin");
  const { view, error, reload } = useGithubProject(project.project_id);
  const { busy, run } = useGithubRun();
  const [settings, setSettings] = useState(false);
  const repo = view?.repo ?? null;
  const name = repo ? `${repo.owner}/${repo.name}` : project.name;
  const act: Run = (action, label, done) => run(action, label, done).then((ok) => { reload(); return ok; });
  const githubSettings = useGithubSettings(view, reload);
  const off = view?.settings.mode === "off";
  return (
    <Screen title={shown === "find_fix" ? "Find & fix" : "Issues"} subtitle={name} className="settings-screen github-screen"
      actions={view && <IconButton icon={Settings} label="GitHub settings" onClick={() => setSettings(true)} />}>
      <div className="settings-stack">
        {shown === "issues" && <IssuesOrPrs projectId={project.project_id} shown="issues" />}
        <Gate what="see this project’s GitHub">
          {error ? <Notice tone="unknown" action={<Button size="sm" disabled={!online} onClick={reload}>Try again</Button>}>{error}</Notice>
            : !view ? <div className="screen__loading" aria-busy="true" aria-label="Loading GitHub" />
              : <>
                <Problems view={view} reload={reload} />
                {repo && (off
                  ? <TurnOn settings={githubSettings} disabled={!online || !admin} />
                  : shown === "issues" && <MergeSwitch view={view} settings={githubSettings} online={online} admin={admin} busy={busy} run={act} />)}
                {shown === "find_fix"
                  ? <FindFixView project={project} off={off || !repo} />
                  : repo && <IssuesView project={project} view={view} off={off} />}
              </>}
        </Gate>
      </div>
      <SettingsSheet open={settings} onClose={() => setSettings(false)} project={project} view={view} error={error} busy={busy} run={act} reload={reload} settings={githubSettings} />
    </Screen>
  );
  function Problems({ view, reload }: { view: GithubProjectView; reload: () => void }) {
    return <>
      {!view.login && (
        <Notice tone="warn" icon={KeyRound} title="No GitHub account"
          action={<Button size="sm" onClick={() => ui.push({ screen: "accounts" })}>Open Accounts</Button>}>
          Sign in to GitHub under Accounts on {profile.name} first. The agent works as that account.
        </Notice>
      )}
      {view.problem && <Notice tone="unknown" action={<Button size="sm" disabled={!online} onClick={reload}>Try again</Button>}>{view.problem}</Notice>}
    </>;
  }
}

const MERGING = "Letting the agent merge once checks pass";
const NOT_MERGING = "Leaving merging to you";

/** Whether the agent merges its own pull requests once their checks pass. Turning it on warns first, once. */
function MergeSwitch({ view, settings, online, admin, busy, run }: {
  view: GithubProjectView; settings: GithubSettingsControl; online: boolean; admin: boolean; busy: string | null; run: Run;
}) {
  const repo = view.repo!;
  const s = settings.inForce;
  const branch = repo.default_branch ?? "main";
  const name = `${repo.owner}/${repo.name}`;
  const on = s.mode === "through_merge";
  const [asking, setAsking] = useState(false);
  const warn = useWarning("github.merge");
  // Said yes in the sheet: turning it on and answering its warning are two
  // steps, and the warning card would show between them until the answer
  // reaches the view. It shows again if the answer doesn't go through.
  const [said, setSaid] = useState(false);
  useEffect(() => { if (s.warning_answered || !on) setSaid(false); }, [s.warning_answered, on]);
  const title = useId(), hint = useId();
  const may = repo.admin || on;
  // It shows what is in force, with the change beside it as changing.
  const merge = (to: boolean) => settings.set({ ...settings.value, mode: to ? "through_merge" : "drafts" },
    { label: to ? MERGING : NOT_MERGING, what: "whether the agent merges", ...(to ? {} : { done: "You merge the agent’s pull requests" }) });
  const turnOn = async () => {
    setSaid(true);
    setAsking(false);
    let ok = await merge(true);
    if (ok) {
      const a = answerWarning(view, true);
      ok = await run(a.action, MERGING, a.done);
    }
    if (!ok) setSaid(false);
  };
  const answer = (keep: boolean) => { const a = answerWarning(view, keep); void run(a.action, a.label, a.done); };
  return (
    <div className="settings-card settings-stack">
      <label className="github-switch">
        <input type="checkbox" role="switch" checked={on} aria-labelledby={title} aria-describedby={hint}
          disabled={!online || !admin || !may}
          onChange={(e) => (e.target.checked
            ? warn.ask(() => setAsking(true), () => void turnOn())
            : void merge(false))} />
        <span className="github-switch__text">
          <span className="github-switch__title" id={title}>Merge once checks pass</span>
          <span className="settings-meta" id={hint}>
            {on ? reminderText(branch) : "Off: you merge the agent’s pull requests. It opens them as drafts and keeps their checks passing."}
            {!may && ` Offered where ${view.login ? `@${view.login}` : "the account"} administers the repository.`}
            {may && !admin && " This device may not change it."}
          </span>
        </span>
      </label>
      {settings.turning && settings.turning !== "off" && (
        <Changing to={settings.turning === "through_merge" ? "the agent merges once checks pass" : "you merge the agent’s pull requests"} />
      )}
      {on && !s.warning_answered && !said && <ThroughMergeWarning view={view} busy={busy} online={online && admin} onAnswer={answer} />}
      <Sheet open={asking} onClose={() => setAsking(false)} title="Merge once checks pass?" subtitle={name}
        footer={<>
          <Button variant="quiet" onClick={() => setAsking(false)}>Not now</Button>
          <Button variant="primary" disabled={!online} onClick={() => { warn.agreed(); void turnOn(); }}>Merge once checks pass</Button>
        </>}>
        <div className="settings-stack github-warning">
          <p>{warningText(branch, name)}</p>
          <MergeWarnings warnings={view.warnings} />
          <p className="settings-meta">Turn it off here any time. Pull requests from issues someone outside the repository wrote still wait for you.</p>
          {warn.check}
        </div>
      </Sheet>
    </div>
  );
}

// ---- Issues ----------------------------------------------------------------

function IssuesView({ project, view, off }: { project: Project; view: GithubProjectView; off: boolean }) {
  const { profile, ui } = useApp();
  const online = useLink().state === "online";
  const filing = useGithubIssuesSupported();
  const [open, setOpen] = useState<number | null>(null);
  const { items, error, loading, reload } = useGithubItems(project.project_id, "issues");
  const live = useLiveItems(project.project_id, items?.items);
  const groups = live ? groupItems(live) : null;
  const shown = open === null ? undefined : live?.find((i) => i.number === open);
  const rows = (xs: GithubItem[]) => (
    <div className="group">{xs.map((item) => <ItemRow key={item.number} item={item} onOpen={() => setOpen(item.number)} />)}</div>
  );
  return <>
    {groups && items && items.items.length > 0 && <RunAll project={project} view={view} take={groups.take} unsorted={groups.rest} off={off} />}
    <div className="group">
      <ListRow icon={Plus} title="New issue" chevron disabled={off || !filing}
        subtitle={!filing ? GITHUB_ISSUES_UPDATE : "Write it here, then start an agent on it or keep it for later"}
        onClick={() => ui.push({ screen: "github_new", projectId: project.project_id, kind: "issue" })} />
    </div>
    {error && <Notice tone="unknown" action={<Button size="sm" disabled={!online} onClick={reload}>Try again</Button>}>{error}</Notice>}
    {!items && !error && (online
      ? <div className="screen__loading" aria-busy="true" aria-label="Loading issues" />
      : <Notice tone="unknown">Connect to {profile.name} to read GitHub.</Notice>)}
    {groups && items && (items.items.length === 0
      ? <Empty icon={CircleDot} title="No open issues">File one with New issue, or start a review in Find & fix to find some.</Empty>
      : <>
        {groups.needs.length > 0 && <Section title={GROUP_WORDS.needs_you}>{rows(groups.needs)}</Section>}
        {groups.take.length > 0 && <Section title={GROUP_WORDS.agent_can_take}>{rows(groups.take)}</Section>}
        {groups.rest.length > 0 && <Section title={groups.take.length || groups.needs.length ? "Not sorted" : "Open"}>{rows(groups.rest)}</Section>}
        {items.truncated && <p className="settings-meta">More are open than are listed here. See them all on GitHub.</p>}
        {loading && <p className="settings-meta" role="status">Checking GitHub…</p>}
      </>)}
    {shown && <IssueSheet key={shown.number} item={shown} project={project} view={view} off={off} onClose={() => setOpen(null)} />}
  </>;
}

/**
 * One issue. When it waits for the person, the sheet leads with why and the
 * one next step, with buttons that take it: close the issue, try again, or
 * open the agent's task. Otherwise it is who works it, what else to tell
 * them, who reviews the code if anyone, and starting them now, when the
 * project is free, or later.
 */
function IssueSheet({ item, project, view, off, onClose }: { item: GithubItem; project: Project; view: GithubProjectView; off: boolean; onClose: () => void }) {
  const { profile, platform } = useApp();
  const online = useLink().state === "online";
  const openTask = useOpenTask();
  const draft = useDraftGithub(project.project_id);
  const harnesses = useHost((s) => s.harnesses);
  const launchSupported = useHost((s) => !!s.capabilities?.features.includes(LAUNCH_POLICY_FEATURE));
  const writer = useHost((s) => writerHold(s.projects[project.project_id]?.active_writer, s), shallowEqual);
  const task = useHost((s) => (item.task_id ? s.tasks[item.task_id] ?? null : null));
  const need = useNeed(item);
  const closer = useCloseIssue(project, view, item);
  // Run all's coder for the project, else the agent last used here.
  const fixWith = view.settings.workflow.fix_with;
  const lastUsed = useRole(project, "fix");
  const remembered = fixWith ? asAgent(fixWith) : lastUsed;
  const [chosen, setChosen] = useState<AgentChoice | null>(null);
  const [choosing, setChoosing] = useState(false);
  const [direction, setDirection] = useState("");
  const answerField = useRef<HTMLTextAreaElement>(null);
  const [retrying, setRetrying] = useState(false);
  const { sending, start } = useStartTask();
  const agent = chosen ?? remembered;
  const reviewer = useReviewer(project, agent);
  const stranger = !item.author_writes;
  const mode: TaskMode = stranger ? "plan" : "work";
  const waits = writer !== null && mode === "work";
  const blocker = off ? "Turn GitHub on for this project first."
    : agentBlocker(harnesses, agent, mode) ?? (reviewer.sent ? agentBlocker(harnesses, reviewer.agent, "plan") : null);
  const link: GithubLink = { type: "issue", number: item.number, ...(reviewer.sent ? { review_with: reviewer.sent } : {}) };
  // The Sort sent it back with a question, so the agent starts from the
  // person's answer, and is told what was asked: it doesn't see the Sort's note.
  const answering = need?.kind === "sorted";
  const unanswered = answering && !direction.trim();
  const answer = answering && need.said ? `The Sort asked: “${need.said.trim()}”\n\nMy answer: ${direction.trim()}` : direction;
  const text = withDirection(linkText(link, item.title), answer);
  const send = async (placement: TaskPlacement, label: string, done: string) => {
    if (!agent) return;
    const sent = agentSent(agent, launchSupported);
    const body = createTaskCommand({
      projectId: project.project_id, prompt: text, title: null, agent: sent, mode, placement, mission: null, bundle: null, github: link,
    });
    if (await start(body, label, done)) {
      rememberRole(profile.profileId, project.project_id, "fix", asLaunch(sent));
      if (reviewer.sent) rememberRole(profile.profileId, project.project_id, "review", reviewer.sent);
      onClose();
    }
  };
  const disabled = !online || !!blocker || !!sending || unanswered;
  const now = startOrQueue(waits);
  // A task already on it is opened, not started again.
  const on = task && going(task) ? task : null;
  const closes = !!need && CLOSES.includes(need.kind);
  // What the person settles without a new fix keeps the start form folded away until they ask for one.
  const folded = !on && !!need && FOLDED.includes(need.kind) && !retrying && (closes || !!task);
  const open = (t: Task) => { onClose(); openTask(t); };
  const close = (done: boolean) => { if (closer.close(done)) onClose(); };
  const closeButton = (variant: "primary" | "secondary", size?: "sm") => (closer.supported
    ? <Button variant={variant} size={size} disabled={closer.disabled || off} onClick={() => close(need?.kind === "fixed")}>Close issue</Button>
    : <Button variant={variant} size={size} icon={ExternalLink} onClick={() => void platform.openExternal(item.url)}>Close on GitHub</Button>);
  const ask = !need ? "" : retrying && closes ? "Say how to make it happen below, then tap Start. The agent reads it with the issue and what the last agent found."
    : retrying ? "Add anything the agent should know below, then tap Start."
    : need.ask;
  const cardAction = retrying ? <Button variant="quiet" size="sm" onClick={() => setRetrying(false)}>Never mind</Button>
    : need?.kind === "checked_before" ? <Button variant="secondary" size="sm" icon={ExternalLink} onClick={() => void platform.openExternal(item.url)}>Read its comment on GitHub</Button>
    : need?.kind === "sorted" || need?.kind === "held" ? closeButton("secondary", "sm")
    : undefined;
  const footer = on ? <Button variant="primary" onClick={() => open(on)}>Open its task</Button>
    : folded && closes ? <>
      <Button variant="secondary" onClick={() => setRetrying(true)}>It still happens</Button>
      {closeButton("primary")}
    </>
    : folded && task ? <>
      {need?.kind === "failed" && <Button variant="secondary" onClick={() => setRetrying(true)}>Try again</Button>}
      <Button variant="primary" onClick={() => open(task)}>{need?.kind === "question" ? "Answer it" : "Open its task"}</Button>
    </>
    : <>
      <Button variant="secondary" disabled={disabled} pending={sending === QUEUEING}
        onClick={() => void send(nextUp(), QUEUEING, `#${item.number} is in Next up`)}>
        Add to Next up
      </Button>
      <Button variant="primary" disabled={disabled} pending={sending === STARTING}
        onClick={() => void send(now, STARTING, waits ? `#${item.number} starts when the project is free` : `Started on #${item.number}`)}>
        {waits ? "Start when free" : "Start"}
      </Button>
    </>;
  return (
    <Sheet open onClose={onClose} title={`#${item.number} ${item.title}`} subtitle={`Issue by @${item.author}`} footer={footer}>
      <div className="settings-stack">
        {need && <NeedCard need={need} ask={ask} action={cardAction} />}
        {answering && !on && item.triage_pick && <div className="github-pick">
          <p className="settings-meta">The Sort would answer: {item.triage_pick}</p>
          {/* A start for the person's own answer, never a one-tap accept: they still read it and tap Start. */}
          {!direction.trim() && <Button variant="quiet" size="sm" onClick={() => {
            const pick = item.triage_pick ?? "";
            setDirection(pick);
            requestAnimationFrame(() => { const field = answerField.current; field?.focus(); field?.setSelectionRange(pick.length, pick.length); });
          }}>Use as my answer</Button>}
        </div>}
        {answering && !on && <TextArea ref={answerField} label="Your answer" value={direction} onChange={setDirection} rows={2} maxRows={8} aria-required
          hint="Needed to start. The agent is sent the Sort’s question and your answer, and reads the issue on GitHub." />}
        {need && closes && !closer.supported && <p className="settings-meta">{GITHUB_CLOSE_UPDATE}</p>}
        {stranger && !folded && (
          <Notice tone="info" title="From someone who can’t write to the repository">
            The agent reads what they wrote as information, not instructions, and starts in Plan. Its plan needs your OK before it changes anything.
          </Notice>
        )}
        {!need && item.triage_note && <p>{item.triage_note}</p>}
        {on && <Waiting task={on} project={project} onClose={onClose} />}
        {!on && !folded && <div className="group">
          <ListRow icon={Bot} title="Worked by" subtitle={agent ? agentLabel(harnesses, agent) : "No agent can start work yet"} chevron
            onClick={() => setChoosing(true)} disabled={!agent} />
        </div>}
        {!on && !folded && <>
          {agent && <LaunchLine className="settings-meta" harness={agent.harness} policy={launchSupported ? agent.launchPolicy : null} mode={mode} />}
          {retrying && closes
            ? <TextArea label="How to make it happen" value={direction} onChange={setDirection} rows={3} maxRows={8}
              hint="What you do, and what you see. The agent reads this, the issue, and the last agent’s comment on it." />
            : answering ? null
            : <TextArea label="More direction" value={direction} onChange={setDirection} rows={2} maxRows={6}
              hint={`Optional. The agent is asked “${linkText(link, item.title)}” and reads the issue on GitHub; add anything else it should know.`} />}
          <ReviewerPick project={project} reviewer={reviewer} />
          {blocker ? <p className="settings-meta" role="status">{blocker}</p>
            : unanswered ? <p className="settings-meta" role="status">Start and Add to Next up wait for your answer above.</p> : <>
            <p className="settings-meta">
              {mode === "work" ? "It makes a branch, fixes the issue and opens a draft pull request that says “Closes” only if the fix covers it." : "It plans the fix without changing files."}
            </p>
            {/* What it waits for is its own note, apart from what it does. */}
            {waits && writer && <p className="settings-meta" role="status">{writerHoldText(writer, project.name)} Start when free starts it then.</p>}
          </>}
        </>}
        {!folded && <MoveItem item={item} project={project} off={off} />}
        <div className="github-list__actions">
          {!on && !folded && <Button variant="quiet" size="sm" disabled={off} onClick={() => { if (draft({ link, text, agent, plan: stranger })) onClose(); }}>Edit first</Button>}
          {task && !on && (closes || !folded) && <Button variant="quiet" size="sm" onClick={() => open(task)}>Open its task</Button>}
          <Button variant="quiet" size="sm" icon={ExternalLink} onClick={() => void platform.openExternal(item.url)}>Open on GitHub</Button>
        </div>
      </div>
      {agent && (
        <AgentSheet open={choosing} onClose={() => setChoosing(false)} project={project} value={agent} mode={mode}
          launchFlags={launchSupported} onChange={setChosen} />
      )}
    </Sheet>
  );
}

const STARTING = "Starting work on an issue";
const QUEUEING = "Adding an issue to Next up";
const MOVING = "Moving an issue";

type Place = TriageGroup | "unsorted";
const PLACES: { value: Place; label: string }[] = [
  { value: "agent_can_take", label: "Agent can take it" }, { value: "needs_you", label: "Needs you" }, { value: "unsorted", label: "Not sorted" },
];

/** Where the issue is listed, which the person can change; a later sort leaves it there. */
function MoveItem({ item, project, off }: { item: GithubItem; project: Project; off: boolean }) {
  const { session } = useApp();
  const online = useLink().state === "online";
  const allowed = useMay("tasks");
  const supported = useHost((s) => !!s.capabilities?.features.includes(GITHUB_RUN_FEATURE));
  const { run } = useGithubRun();
  const [place, setPlace] = useState<Place>(item.group ?? "unsorted");
  useEffect(() => setPlace(item.group ?? "unsorted"), [item.group]);
  const latest = useRef(0);
  if (!supported) return null;
  // The list moves it at once; if the computer refuses, it goes back,
  // unless another move has been made over it since.
  const move = async (to: Place) => {
    const was = place, mine = ++latest.current;
    setPlace(to);
    const settled = expectItem(session, project.project_id, item.number, { group: to === "unsorted" ? null : to });
    const ok = await run({ action: "set_group", project_id: project.project_id, number: item.number, pr: item.pr, ...(to === "unsorted" ? {} : { group: to }) },
      MOVING, `#${item.number} is in ${PLACES.find((p) => p.value === to)!.label}`);
    settled(ok);
    if (!ok && latest.current === mine) setPlace(was);
  };
  return <>
    <Segmented className="segmented--fit" label="Listed under" value={place} options={PLACES.map((p) => ({ ...p, disabled: !online || !allowed || off }))}
      onChange={(to) => void move(to)} />
    <p className="settings-meta">
      {place === "needs_you" ? "It waits for you: Run all leaves it alone, and one waiting in Next up is taken out."
        : place === "agent_can_take" ? "Run all fixes it in turn."
        : "The next sort decides."}
    </p>
  </>;
}

/** A task that waits in Next up for its turn or for a go-ahead, or works on the issue. */
function Waiting({ task, project, onClose }: { task: Task; project: Project; onClose: () => void }) {
  const { session, ui, profile } = useApp();
  const online = useLink().state === "online";
  const allowed = useMay("tasks");
  const { queue } = useQueue(project.project_id);
  const [sending, setSending] = useState(false);
  const item = pendingQueueItems(queue).find((i) => i.task_id === task.task_id);
  if (task.state !== "accepted") return <Notice tone="info" title="An agent is on it">{task.status_line ?? "Working"}</Notice>;
  if (!item || !queue) return <Notice tone="info" title="Starting">It starts as soon as the computer gets to it.</Notice>;
  const first = pendingQueueItems(queue)[0]?.item_id === item.item_id;
  const asks = item.state.state === "awaiting_start" || item.authorization !== "auto_start";
  const letGo = async () => {
    setSending(true);
    try {
      await commandResult(session, await session.command({ command: "queue_set_authorization", item_id: item.item_id, authorization: "auto_start" },
        { label: "Letting an issue start by itself", expectedRevision: queue.revision }));
      toast("It starts by itself in its turn");
    } catch (e) { toast(describeError(e, profile.name), { tone: "stop" }); } finally { setSending(false); }
  };
  return (
    <Notice tone={asks ? "warn" : "info"} title={asks ? "Waiting for your go-ahead" : "Waiting in Next up"}
      action={asks && <div className="github-list__actions">
        <Button size="sm" variant="secondary" disabled={!online || !allowed || sending}
          onClick={() => { onClose(); ui.push({ screen: "review", projectId: project.project_id, taskId: task.task_id, queueItemId: item.item_id }); }}>Review and start</Button>
        <Button size="sm" variant="primary" disabled={!online || !allowed} pending={sending} onClick={() => void letGo()}>Let it start by itself</Button>
      </div>}>
      {item.state.state === "blocked" ? item.state.reason
        : asks ? `It’s ${first ? "first" : `number ${pendingQueueItems(queue).indexOf(item) + 1}`} in Next up and starts once you say so${first ? "; nothing after it starts until then" : ""}.`
        : first ? "It starts by itself as soon as the project is free." : "It starts by itself when the ones before it are done."}
    </Notice>
  );
}

// ---- Find & fix --------------------------------------------------------------

/** How many of a kind of task Find & fix lists; the rest are in Work’s Finished and Archived. */
const SHOWN = 8;

const isReview = (t: Task) => t.github?.link.type === "find" || t.github?.link.type === "triage";

/**
 * Starting a review that fixes what it files or only files it, fixing the open
 * issues, or sorting them, and the reviews and fixes so far. The same works
 * without GitHub, in a file, for a project GitHub is off for too.
 */
function FindFixView({ project, off }: { project: Project; off: boolean }) {
  const { ui } = useApp();
  const fileSupported = useFindingsSupported();
  const online = useLink().state === "online";
  const draft = useDraftGithub(project.project_id);
  const sorter = useRole(project, "find");
  const reviews = useHost((s) => projectTasks(s, project.project_id).filter((t) => isReview(t) && t.state !== "archived").slice(0, SHOWN), shallowEqual);
  const fixes = useHost((s) => projectTasks(s, project.project_id).filter((t) => !!t.github?.found_by && t.state !== "archived").slice(0, SHOWN), shallowEqual);
  const more = useHost((s) => projectTasks(s, project.project_id).filter((t) => isReview(t) && t.state !== "archived").length > SHOWN);
  return <>
    <div className="group">
      <ListRow icon={Search} title="Review and fix" chevron disabled={off}
        subtitle="An agent reviews the project and files what it finds, for another to fix"
        onClick={() => ui.push({ screen: "github_new", projectId: project.project_id, kind: "review" })} />
      <ListRow icon={FileSearch} title="Review only" chevron disabled={off}
        subtitle="An agent files what it finds as issues and nothing more"
        onClick={() => ui.push({ screen: "github_new", projectId: project.project_id, kind: "review_only" })} />
      <ListRow icon={Wrench} title="Fix only" chevron disabled={off}
        subtitle="Start agents on the open issues, all at once or one at a time"
        onClick={() => ui.push({ screen: "github", projectId: project.project_id, view: "issues" })} />
      <ListRow icon={ListChecks} title="Sort open issues" chevron disabled={off || !online}
        subtitle="An agent sorts what it can take from what needs you"
        onClick={() => draft({ link: { type: "triage" }, text: linkText({ type: "triage" }), agent: sorter })} />
    </div>
    <div className="group">
      <ListRow icon={FileText} title="Without GitHub" chevron disabled={!fileSupported}
        subtitle={fileSupported ? `The same in ${FINDINGS_FILE}: review, fix, or both, with nothing on GitHub` : `Update ${PRODUCT_NAME} on the computer to keep findings in a file`}
        onClick={() => ui.push({ screen: "findings", projectId: project.project_id })} />
    </div>
    {reviews.length > 0 ? (
      <Section title="Reviews">
        <div className="group">{reviews.map((t) => <TaskRow key={t.task_id} task={t} />)}</div>
        {more && <p className="settings-meta">Older reviews are in Tasks, under Finished and Archived.</p>}
      </Section>
    ) : !off && <Empty icon={Search} title="No reviews yet">Start one with Review and fix or Review only. It reads the project, or uses the app on its screen, without changing anything, and files the issues it finds.</Empty>}
    {fixes.length > 0 && (
      <Section title="Fixes from reviews">
        <div className="group">{fixes.map((t) => <TaskRow key={t.task_id} task={t} />)}</div>
      </Section>
    )}
  </>;
}
