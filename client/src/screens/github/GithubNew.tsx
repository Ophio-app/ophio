// SPDX-License-Identifier: Apache-2.0
//
// The two GitHub things New starts besides a task, each on its own screen:
// a new issue, filed in the person's name and worked now, later or not at
// all, and a new review, where one agent reviews the project, by reading it
// or using it, and files issues for another to fix.

import { useEffect, useId, useState } from "react";
import { AppWindow, Bot, KeyRound, Minus, Monitor, MousePointer2, Plus, Search, Wrench } from "lucide-react";
import { describeError, shallowEqual, useApp, useHost, useLink, useUi } from "../../app/context";
import {
  FIX_HINT, FIX_WORDS, FOCUS_HINT, FOCUS_WORDS, MAX_ISSUES, OPEN_DIRECTION, USERS, fixSent, focusesFor, lastLimit, linkText, rememberLimit, rememberRole, useGithubAct,
  useGithubIssuesSupported, useGithubProject, useGithubReviewSupported, useGithubUatSupported, usesIt,
} from "../../app/github";
import { GITHUB_ISSUES_UPDATE, GITHUB_REVIEW_UPDATE } from "../../app/githubFeature";
import { FIX_LAUNCH_FEATURE, LAUNCH_POLICY_FEATURE } from "../../app/launchPolicy";
import { LaunchLine } from "../talk/LaunchLine";
import type { AgentChoice, IssueDraft, IssueThen } from "../../app/ui";
import { Button, IconButton } from "../../design/Button";
import { Chip, Segmented } from "../../design/Chip";
import { TextArea, TextField } from "../../design/Field";
import { ListRow, Section } from "../../design/ListRow";
import { Notice, availabilityText } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import { useWarning } from "../../app/warnings";
import type {
  Command, ControlTarget, FindFocus, FindScope, FixFindings, GithubLink, GithubProjectView, HarnessInfo, Project,
} from "../../protocol/types";
import { useMay } from "../../shell/NotAllowed";
import { Screen } from "../../shell/Screen";
import { AgentSheet } from "../talk/AgentSheet";
import { agentLabel, createTaskCommand, writerActivity, writerHold, writerHoldText } from "../talk/intent";
import { agentUnavailable, targetBlocker } from "../control/computerReview";
import { Unlock, useLocked } from "../control/Unlock";
import type { RouteProps } from "../types";
import { Gate, TurnOn, agentBlocker, agentSent, startOrQueue, asLaunch, useDraftGithub, useGithubSettings, useRole, useStartTask, useUser, withDirection } from "./common";
import { ReviewerPick, useReviewer } from "./Reviewer";
import "../settings/settings.css";

export default function GithubNew({ route }: RouteProps<"github_new">) {
  const { profile, ui } = useApp();
  const online = useLink().state === "online";
  const admin = useMay("admin");
  const project = useHost((s) => s.projects[route.projectId] ?? null);
  const { view, error, reload } = useGithubProject(project ? project.project_id : null);
  const settings = useGithubSettings(view, reload);
  const repo = view?.repo ?? null;
  const issue = route.kind === "issue";
  const only = route.kind === "review_only";
  const failed = error && <Notice tone="unknown" action={<Button size="sm" disabled={!online} onClick={reload}>Try again</Button>}>{error}</Notice>;
  return (
    <Screen title={issue ? "New issue" : only ? "Review only" : "Review and fix"} subtitle={repo ? `${repo.owner}/${repo.name}` : project?.name ?? profile.name}
      className="settings-screen github-screen">
      <div className="settings-stack">
        <Gate what={issue ? "file issues" : "start reviews"}>
          {!project ? <Notice tone="unknown">This project isn’t on {profile.name} any more.</Notice>
            : !view ? failed || <div className="screen__loading" aria-busy="true" aria-label="Loading GitHub" />
              // A refresh that fails leaves what was read before, and what's being written, in place.
              : <>
                  {failed}
                  {!view.login && (
                    <Notice tone="warn" icon={KeyRound} title="No GitHub account"
                      action={<Button size="sm" onClick={() => ui.push({ screen: "accounts" })}>Open Accounts</Button>}>
                      Sign in to GitHub under Accounts on {profile.name} first. The agent works as that account.
                    </Notice>
                  )}
                  {!repo ? <Notice tone="unknown" action={<Button size="sm" disabled={!online} onClick={reload}>Try again</Button>}>
                    {view.problem ?? "This project’s repository isn’t on GitHub."}
                  </Notice>
                    : view.settings.mode === "off" ? <TurnOn settings={settings} disabled={!online || !admin} />
                      : issue ? <NewIssue project={project} view={view} /> : <NewReview project={project} view={view} only={only} />}
                </>}
        </Gate>
      </div>
    </Screen>
  );
}

// ---- New issue ---------------------------------------------------------------

type Then = IssueThen;
const THEN_WORDS: Record<Then, string> = { start: "Start work", next_up: "Add to Next up", file: "Just file it" };
const MAX_TITLE = 256;
const MAX_BODY = 60_000;
const FILING = "Filing an issue";
const STARTING = "Starting work on an issue";

// A text field drops line breaks itself, so only the length needs saying.
const titleError = (t: string) => t.trim().length > MAX_TITLE ? `${MAX_TITLE} characters at most` : null;

/**
 * Writes an issue and files it on GitHub in the person's name, then starts
 * an agent on it, adds it to Next up, or leaves it. Filed is filed: if the
 * start fails after, trying again only starts the work. What's written is
 * kept for the project on this phone until it's done with or discarded,
 * through a failed refresh, Back or a restart.
 */
function NewIssue({ project, view }: { project: Project; view: GithubProjectView }) {
  const { profile, ui } = useApp();
  const online = useLink().state === "online";
  const supported = useGithubIssuesSupported();
  const act = useGithubAct();
  const harnesses = useHost((s) => s.harnesses);
  const launchSupported = useHost((s) => !!s.capabilities?.features.includes(LAUNCH_POLICY_FEATURE));
  const writer = useHost((s) => writerHold(s.projects[project.project_id]?.active_writer, s), shallowEqual);
  const remembered = useRole(project, "fix");
  const pid = project.project_id;
  const draft = useUi((s) => s.issueDrafts[pid] ?? null);
  const keep = (patch: Partial<Omit<IssueDraft, "id" | "updatedAt">>) => ui.saveIssueDraft(pid, patch);
  const { title = "", body = "", then = "start", agent: chosen = null, reviewOn = false, reviewer: reviewerChosen = null, filed = null, pending = null } = draft ?? {};
  const [choosing, setChoosing] = useState(false);
  const [discarding, setDiscarding] = useState(false);
  const warn = useWarning("draft.discard");
  const { sending, start } = useStartTask();
  const agent = chosen ?? remembered;
  const reviewer = useReviewer(project, agent, {
    on: reviewOn, agent: reviewerChosen, set: (r) => keep({ ...(r.on !== undefined ? { reviewOn: r.on } : {}), ...(r.agent !== undefined ? { reviewer: r.agent } : {}) }),
  });
  const works = then !== "file";
  const waits = writer !== null && then === "start";
  const repo = view.repo!;
  const tooLong = body.length > MAX_BODY ? `The description is ${body.length - MAX_BODY} characters over GitHub’s limit.` : null;
  const blocker = !supported ? GITHUB_ISSUES_UPDATE
    : !works ? null
      : agentBlocker(harnesses, agent, "work") ?? (reviewer.sent ? agentBlocker(harnesses, reviewer.agent, "plan") : null);
  const ready = !!title.trim() && !titleError(title) && !tooLong;
  // On its way, even from a New issue left since: it can't go twice.
  const busy = (!!pending && !pending.unknown) || !!sending;

  const go = async () => {
    if (!draft || busy) return;
    // What answers later changes only this draft, and leaves only this screen.
    const id = draft.id;
    const mine = (patch: Partial<Omit<IssueDraft, "id" | "updatedAt">>) => ui.saveIssueDraft(pid, patch, id);
    const leave = () => {
      const r = ui.route();
      if (r.screen === "github_new" && r.projectId === pid && r.kind === "issue") ui.back();
    };
    let issue = filed;
    if (!issue) {
      // Kept before it leaves, so a restart part way through says it may be filed.
      mine({ pending: { what: "filing", unknown: false } });
      ui.persistNow();
      try {
        const made = await act({ action: "file_issue", project_id: pid, repo: `${repo.owner}/${repo.name}`, title: title.trim(), body }, FILING);
        if (made?.kind !== "issue_filed") throw new Error("GitHub didn’t say which issue it filed. Look for it on GitHub before filing it again.");
        issue = { number: made.number, url: made.url };
        mine({ filed: issue, pending: null });
        ui.persistNow();
      } catch (e) {
        mine({ pending: null });
        toast(e instanceof Error ? e.message : String(e), { tone: "stop" });
        return;
      }
    }
    if (!works) {
      toast(`Filed #${issue.number}`);
      ui.clearIssueDraft(pid, id);
      leave();
      return;
    }
    if (!agent) return;
    const link: GithubLink = { type: "issue", number: issue.number, ...(reviewer.sent ? { review_with: reviewer.sent } : {}) };
    const sent = agentSent(agent, launchSupported);
    const queued = then === "next_up" || waits;
    const placement = startOrQueue(queued);
    const done = then === "next_up" ? `Filed #${issue.number} and added it to Next up`
      : waits ? `Filed #${issue.number}; it starts when the project is free` : `Filed #${issue.number} and started on it`;
    const command = createTaskCommand({
      projectId: project.project_id, prompt: linkText(link, title.trim()), title: null, agent: sent, mode: "work", placement,
      mission: null, bundle: null, github: link,
    });
    mine({ pending: { what: "starting", unknown: false } });
    ui.persistNow();
    if (await start(command, STARTING, done)) {
      rememberRole(profile.profileId, pid, "fix", asLaunch(sent));
      if (reviewer.sent) rememberRole(profile.profileId, pid, "review", reviewer.sent);
      ui.clearIssueDraft(pid, id);
      leave();
    } else mine({ pending: null });
  };

  const discard = () => { setDiscarding(false); if (draft) ui.clearIssueDraft(pid, draft.id); };

  const label = filed
    ? (then === "next_up" ? `Add #${filed.number} to Next up` : then === "start" ? (waits ? `Start #${filed.number} when free` : `Start work on #${filed.number}`) : "Done")
    : then === "next_up" ? "File and add to Next up" : then === "start" ? (waits ? "File and start when free" : "File and start") : "File issue";
  return <>
    {!supported && <Notice tone="info">{GITHUB_ISSUES_UPDATE}</Notice>}
    {pending?.unknown ? (
      pending.what === "filing"
        ? <Notice tone="unknown" title="It may be filed already">The app closed before GitHub said. Look for it on GitHub before filing it again.</Notice>
        : <Notice tone="unknown" title={filed ? `#${filed.number} is filed on GitHub` : undefined}>
          The app closed before {profile.name} said whether its work started. Look in Tasks before starting it again.
        </Notice>
    ) : filed && (
      <Notice tone="info" title={`#${filed.number} is filed on GitHub`}>
        {works ? "The work didn’t start. Try again to start it; the issue won’t be filed twice." : "It’s on GitHub."}
      </Notice>
    )}
    <div className="settings-stack">
      <TextField label="Title" value={title} onChange={(t) => keep({ title: t })} disabled={!!filed || busy} maxLength={MAX_TITLE + 1} error={titleError(title)}
        autoCapitalize="sentences" />
      <TextArea label="Description" value={body} onChange={(b) => keep({ body: b })} disabled={!!filed || busy} rows={4} maxRows={12}
        hint="What’s wrong or wanted, and how to tell it’s done. Filed as written, in your name." />
      {tooLong && <p className="settings-meta" role="alert">{tooLong}</p>}
    </div>
    <Section title="Then">
      <div className="settings-stack">
        <Segmented label="Then" value={then} onChange={(t) => keep({ then: t })}
          options={(["start", "next_up", "file"] as const).map((v) => ({ value: v, label: THEN_WORDS[v] }))} />
        {works && <>
          <div className="group">
            <ListRow icon={Bot} title="Worked by" subtitle={agent ? agentLabel(harnesses, agent) : "No agent can start work yet"} chevron
              onClick={() => setChoosing(true)} disabled={!agent} />
          </div>
          {agent && <LaunchLine className="settings-meta" harness={agent.harness} policy={launchSupported ? agent.launchPolicy : null} />}
          <ReviewerPick project={project} reviewer={reviewer} />
        </>}
        <p className="settings-meta" role={blocker ? "status" : undefined}>
          {blocker ?? (then === "file" ? "It waits on GitHub, in Issues, until you start it."
            : then === "next_up" ? "It starts by itself when its turn in Next up comes."
              : "It makes a branch, fixes the issue and opens a draft pull request.")}
          {!blocker && waits && writer && ` ${writerHoldText(writer, project.name)} It starts then.`}
        </p>
      </div>
    </Section>
    <div className="github-list__actions">
      <Button variant="primary" disabled={!online || !ready || !!blocker || busy} pending={busy} onClick={() => void go()}>{label}</Button>
      {/* Once filed, it's done with by starting its work, or by Just file it and Done. */}
      {draft && !filed && (
        <Button variant="quiet" disabled={busy} onClick={() => warn.ask(() => setDiscarding(true), discard)}>Discard{warn.more}</Button>
      )}
    </div>
    {agent && (
      <AgentSheet open={choosing} onClose={() => setChoosing(false)} project={project} value={agent} mode="work"
        launchFlags={launchSupported} onChange={(a) => keep({ agent: a })} />
    )}
    <Sheet open={discarding} title="Discard this issue?" onClose={() => setDiscarding(false)}
      footer={<Button variant="stop-outline" block onClick={() => { warn.agreed(); discard(); }}>Discard issue</Button>}>
      <p>Its title, description and choices are removed from this phone. {pending?.unknown
        ? "It may be filed on GitHub already, and stays there if it is." : "Nothing was filed on GitHub."}</p>
      {warn.check}
    </Sheet>
  </>;
}

// ---- New review --------------------------------------------------------------

type Where = "project" | "folder" | "since_last_run";
const WHERE_WORDS: Record<Where, string> = { project: "Whole project", folder: "A folder", since_last_run: "New commits" };
/** What a reviewer who uses the app tries. */
const USE_WORDS: Record<"project" | "since_last_run", string> = { project: "Whole app", since_last_run: "What’s new" };
const badFolder = (path: string) => !path.trim() ? "Name a folder in the project" : /^\/|(^|\/)\.\.(\/|$)/.test(path.trim()) ? "A folder inside the project" : null;
const REVIEWING = "Starting a review";
const MAX_OPEN = 2_000;

/**
 * One agent reviews the project as the chosen kind of reviewer and files
 * issues, most serious first, up to a limit; another checks each is real and
 * fixes it, by itself or when the person starts it. Review only files them
 * and fixes nothing. A reviewer who uses the app does it on a screen, as
 * computer use.
 */
function NewReview({ project, view, only }: { project: Project; view: GithubProjectView; only: boolean }) {
  const { profile, ui } = useApp();
  const online = useLink().state === "online";
  const supported = useGithubReviewSupported();
  const usersSupported = useGithubUatSupported();
  const fixLaunch = useHost((s) => !!s.capabilities?.features.includes(FIX_LAUNCH_FEATURE));
  const launchSupported = useHost((s) => !!s.capabilities?.features.includes(LAUNCH_POLICY_FEATURE));
  const paused = useHost((s) => !!s.queues[project.project_id]?.paused);
  const harnesses = useHost((s) => s.harnesses);
  const draft = useDraftGithub(project.project_id);
  const { sending, start } = useStartTask();
  const rememberedFinder = useRole(project, "find");
  const rememberedFixer = useRole(project, "fix");
  const rememberedUser = useUser(project);
  const [focus, setFocus] = useState<FindFocus | null>(null);
  const [where, setWhere] = useState<Where>("project");
  const [folder, setFolder] = useState("");
  const [finderChosen, setFinder] = useState<AgentChoice | null>(null);
  const [fixerChosen, setFixer] = useState<AgentChoice | null>(null);
  const [userChosen, setUser] = useState<AgentChoice | null>(null);
  const [choosing, setChoosing] = useState<"find" | "fix" | "use" | null>(null);
  const [most, setMostNow] = useState(() => lastLimit(profile.profileId, project.project_id) ?? view.settings.workflow.findings_per_run);
  const setMost = (n: number) => { setMostNow(n); rememberLimit(profile.profileId, project.project_id, n); };
  // Review and fix always fixes; a project that only files by default queues the fixes.
  const [then, setThen] = useState<FixFindings>(() => only ? "file_only" : view.settings.workflow.fix_findings === "file_only" ? "ask_first" : view.settings.workflow.fix_findings);
  const [opening, setOpening] = useState("");
  const uses = usesIt(focus);
  const screen = useScreen(project, uses);
  const finder = finderChosen ?? rememberedFinder;
  const fixer = fixerChosen ?? rememberedFixer;
  const user = userChosen ?? rememberedUser;
  const mostId = useId();
  const usersId = useId();
  const after = supported ? then : view.settings.workflow.fix_findings;
  const fixes = after !== "file_only";
  const folderError = where === "folder" && !uses ? badFolder(folder) : null;
  const reviewer = uses ? screen.blocker ?? userBlocker(harnesses, user) : agentBlocker(harnesses, finder, "plan");
  // A review that fixes nothing still names a fixer, which nothing starts: its reviewer.
  const fixWith = fixer ?? (only ? (uses ? user : finder) : null);
  const blocker = (only && !supported ? GITHUB_REVIEW_UPDATE : null) ?? reviewer ?? (fixes ? agentBlocker(harnesses, fixer, "work") : null)
    ?? (fixWith ? null : "Choose who fixes.");
  const choose = (f: FindFocus | null) => {
    setFocus(f);
    // Someone using the app tries all of it or what's new, not a folder.
    if (usesIt(f) && where === "folder") setWhere("project");
  };
  const link = (): GithubLink | null => {
    if (!fixWith) return null;
    const scope: FindScope = where === "folder" ? { scope: "folder", path: folder.trim() } : { scope: where };
    return {
      type: "find", scope, ...(focus ? { focus } : {}), fix_with: fixSent(asLaunch(fixWith), fixLaunch),
      ...(supported ? { max_issues: most, fix_findings: then } : {}),
    };
  };
  const begin = async () => {
    const l = link();
    if (!l) return;
    if (uses) {
      if (!user || !screen.target) return;
      const body: Command = {
        command: "create_task", agent_chosen: true, project_id: project.project_id, kind: "computer_use", mode: "work", title: null,
        prompt: withDirection(linkText(l), opening.trim() ? `${OPEN_DIRECTION}${opening.trim()}` : ""), bundle: null,
        harness: user.harness, model: user.model, effort: null, github: l, mission: null, target_id: screen.target.target_id,
        placement: { mode: "start_now" },
      };
      if (await start(body, REVIEWING, "Review started")) {
        rememberRole(profile.profileId, project.project_id, "use", asLaunch(user));
        if (fixes && fixer) rememberRole(profile.profileId, project.project_id, "fix", asLaunch(fixer));
        ui.back();
      }
      return;
    }
    if (!finder) return;
    const agent = agentSent(finder, launchSupported);
    const body = createTaskCommand({
      projectId: project.project_id, prompt: linkText(l), title: null, agent, mode: "plan", placement: { mode: "start_now" }, mission: null, bundle: null, github: l,
    });
    if (await start(body, REVIEWING, "Review started")) {
      rememberRole(profile.profileId, project.project_id, "find", asLaunch(agent));
      if (fixes && fixer) rememberRole(profile.profileId, project.project_id, "fix", asLaunch(fixer));
      ui.back();
    }
  };
  const focuses = focusesFor(supported);
  return <>
    {!supported && !only && <Notice tone="info">{GITHUB_REVIEW_UPDATE}</Notice>}
    <Section title="Reviewer">
      <div className="settings-stack">
        <div className="github-focus" role="group" aria-label="Reviewers who read the code">
          <Chip selected={focus === null} onClick={() => choose(null)}>Default</Chip>
          {focuses.map((f) => <Chip key={f} selected={focus === f} onClick={() => choose(f)}>{FOCUS_WORDS[f]}</Chip>)}
        </div>
        {usersSupported && <>
          <span className="github-focus__label" id={usersId}>Uses the app</span>
          <div className="github-focus" role="group" aria-labelledby={usersId}>
            {USERS.map((f) => <Chip key={f} selected={focus === f} onClick={() => choose(f)}>{FOCUS_WORDS[f]}</Chip>)}
          </div>
        </>}
        <p className="settings-meta">{FOCUS_HINT[focus ?? "default"]}</p>
      </div>
    </Section>
    {uses ? (
      <Section title="What it uses">
        <div className="settings-stack">
          <Segmented label="What it uses" value={where === "since_last_run" ? "since_last_run" : "project"} onChange={setWhere}
            options={(["project", "since_last_run"] as const).map((w) => ({ value: w, label: USE_WORDS[w] }))} />
          <p className="settings-meta">{where === "since_last_run"
            ? "What the commits since this project’s last review changed, or the recent ones if there wasn’t one."
            : "Everything a person would do with the app, start to finish."}</p>
          <TextArea label="How to open it" value={opening} onChange={setOpening} rows={2} maxRows={6} maxLength={MAX_OPEN}
            hint="Optional: a web address, or how you start it. Without it, the agent works out how from the project and starts it itself." />
        </div>
      </Section>
    ) : (
      <Section title="What it reviews">
        <div className="settings-stack">
          <Segmented label="What it reviews" value={where} onChange={setWhere}
            options={(["project", "folder", "since_last_run"] as const).map((w) => ({ value: w, label: WHERE_WORDS[w] }))} />
          {where === "folder" && (
            <TextField label="Folder" value={folder} onChange={setFolder} placeholder="src/payments" autoComplete="off" autoCapitalize="off"
              spellCheck={false} error={folder ? folderError : null} />
          )}
          {where === "since_last_run" && <p className="settings-meta">The commits since this project’s last review, or the recent ones if there wasn’t one.</p>}
        </div>
      </Section>
    )}
    {uses && <ScreenPick screen={screen} />}
    <Section title="Agents">
      <div className="group">
        {uses
          ? <ListRow icon={MousePointer2} title="Uses it" subtitle={user ? agentLabel(harnesses, user) : "No agent here can use a screen"} chevron
            disabled={!user} onClick={() => setChoosing("use")} />
          : <ListRow icon={Search} title="Reviews" subtitle={finder ? `${agentLabel(harnesses, finder)} · reads only` : "No agent can start work yet"} chevron
            disabled={!finder} onClick={() => setChoosing("find")} />}
        {!only && (
          <ListRow icon={Wrench} title="Fixes" chevron disabled={!fixer} onClick={() => setChoosing("fix")}
            subtitle={fixer ? agentLabel(harnesses, fixer) : "No agent can start work yet"} />
        )}
      </div>
      {fixes && fixer && (fixLaunch ? <LaunchLine className="settings-meta" harness={fixer.harness} policy={fixer.launchPolicy} /> : <p className="settings-meta">The fixer keeps its own permission setting.</p>)}
      {fixes && !supported && <p className="settings-meta">{FIX_HINT[after]}</p>}
    </Section>
    {supported && (
      <Section title="Issues it files">
        <div className="settings-stack">
          <div className="github-stepper" role="group" aria-labelledby={mostId}>
            <span id={mostId} className="github-stepper__label">Most issues to file</span>
            <IconButton icon={Minus} label="One fewer" disabled={most <= 1} onClick={() => setMost(most - 1)} />
            <output className="github-stepper__value num" aria-live="polite">{most}</output>
            <IconButton icon={Plus} label="One more" disabled={most >= MAX_ISSUES} onClick={() => setMost(most + 1)} />
          </div>
          {!only && (
            <Segmented label="After filing" value={then} onChange={setThen}
              options={(["by_itself", "ask_first"] as const).map((v) => ({ value: v, label: FIX_WORDS[v] }))} />
          )}
          <p className="settings-meta">
            {`It files up to ${most}, most serious first, then stops. `}{FIX_HINT[then]}
            {then === "by_itself" && paused && " Next up is paused, so starting this review resumes it."}
            {then === "ask_first" && paused && " Next up is paused, so they wait until you resume it."}
          </p>
        </div>
      </Section>
    )}
    {blocker && <p className="settings-meta" role="status">{blocker}</p>}
    <div className="github-list__actions">
      <Button variant="primary" icon={uses ? MousePointer2 : Search} disabled={!online || !!blocker || !!folderError || !!sending}
        pending={sending === REVIEWING} onClick={() => void begin()}>Start review</Button>
      {!uses && (
        <Button variant="quiet" disabled={!!folderError || !finder || !fixWith}
          onClick={() => { const l = link(); if (l) draft({ link: l, agent: finder }); }}>Edit first</Button>
      )}
    </div>
    {finder && (
      <AgentSheet open={choosing === "find"} onClose={() => setChoosing(null)} project={project} value={finder} mode="plan"
        launchFlags={launchSupported} onChange={setFinder} />
    )}
    {user && (
      <AgentSheet open={choosing === "use"} onClose={() => setChoosing(null)} project={project} value={user} mode="work"
        computerUse onChange={setUser} />
    )}
    {fixer && (
      <AgentSheet open={choosing === "fix"} onClose={() => setChoosing(null)} project={project} value={fixer} mode="work"
        launchFlags={fixLaunch} onChange={setFixer} />
    )}
  </>;
}

/** Why `user` can't use the app, or null. */
function userBlocker(harnesses: HarnessInfo[], user: AgentChoice | null): string | null {
  if (!user) return "No agent on this computer can use a screen yet. It takes an agent that can use a screen, installed and signed in.";
  const h = harnesses.find((x) => x.kind === user.harness);
  return h ? agentUnavailable(h) : "That agent is no longer on this computer.";
}

type Screen = ReturnType<typeof useScreen>;

/**
 * The screens a reviewer who uses the app can work on, asked for while one
 * is chosen, and what stands in the way of starting on the one picked. With
 * one usable screen, it's picked already.
 */
function useScreen(project: Project, active: boolean) {
  const { session, profile } = useApp();
  const online = useLink().state === "online";
  // Agents use the agent browser through its own tools, never as a screen.
  const targets = useHost((s) => s.targets).filter((t) => t.kind !== "browser");
  const controls = useHost((s) => s.control);
  const caps = useHost((s) => s.capabilities);
  const grants = useHost((s) => s.grants);
  const held = useHost((s) => writerHold(s.projects[project.project_id]?.active_writer, s), shallowEqual);
  const [picked, setPicked] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!active || !online) return;
    let disposed = false;
    void Promise.all([session.refreshHarnesses(), session.query({ kind: "control_targets" }).then((r) => {
      if (!disposed) session.state.set((s) => ({ ...s, targets: r.targets }));
    })]).then(() => { if (!disposed) setError(null); }).catch((e: unknown) => { if (!disposed) setError(describeError(e, profile.name)); });
    return () => { disposed = true; };
  }, [session, online, active, profile.name]);
  const why = (t: ControlTarget) => targetBlocker(caps, t, controls[t.target_id], grants);
  const usable = targets.filter((t) => !why(t));
  const target = targets.find((t) => t.target_id === picked) ?? (usable.length === 1 ? usable[0]! : null);
  const blocker = !grants?.computer_use || !grants.view || !grants.control_input ? "This device doesn’t have permission to start computer use."
    : caps?.computer_use.state !== "available" ? [`Computer use is unavailable on ${profile.name}.`, caps && availabilityText(caps.computer_use).detail].filter(Boolean).join(" ")
      : !target ? (targets.length ? "Choose the screen it uses." : "The computer hasn’t reported a screen it can use.")
        : why(target) ?? (held ? (held.unresolved ? `${writerHoldText(held, project.name)} Check it first.` : `${writerActivity(held, project.name)}. It can start once that’s done.`) : null);
  return { targets, target, pick: setPicked, why, blocker, error };
}

/** The screen the reviewer works on: the real one, so the person's own input
 * there gets in its way. A locked one can be unlocked from here. */
function ScreenPick({ screen }: { screen: Screen }) {
  const locked = useLocked();
  return (
    <Section title="Screen">
      {screen.error && <Notice tone="unknown">{screen.error}</Notice>}
      {screen.targets.length > 0 ? (
        <div className="group" role="radiogroup" aria-label="Screen it uses">
          {screen.targets.map((t) => {
            const why = screen.why(t);
            return <ListRow key={t.target_id} icon={t.kind === "window" ? AppWindow : Monitor} title={t.label}
              subtitle={why ?? `${t.kind === "window" ? "Window" : t.kind === "desktop" ? "Desktop" : "Display"} · ${t.geometry.width} × ${t.geometry.height}`}
              checked={screen.target?.target_id === t.target_id} className={why ? "github-screen--off" : undefined}
              onClick={why ? undefined : () => screen.pick(t.target_id)} />;
          })}
        </div>
      ) : <p className="settings-meta">Turn on desktop sharing on the computer, then come back here.</p>}
      {locked && <Unlock />}
      <p className="settings-meta">It works on the real screen. Using the mouse or keyboard there at the same time gets in its way.</p>
    </Section>
  );
}
