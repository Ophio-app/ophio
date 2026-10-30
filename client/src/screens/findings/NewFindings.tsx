// SPDX-License-Identifier: Apache-2.0
//
// A review without GitHub: one agent reviews the project as the chosen kind
// of reviewer and writes what it finds in FINDINGS.md, and, for Review and
// fix, another waits in Next up and fixes each one once the review is done.
// Both run in Work, as the review writes the file; it is told to change
// nothing else.

import { useId, useState } from "react";
import { Minus, Plus, Search, Wrench } from "lucide-react";
import { describeError, shallowEqual, useApp, useHost, useLink } from "../../app/context";
import { FINDINGS_FILE, type FindWhere, fixAllPrompt, fixAllTitle, reviewPrompt, reviewTitle, useFindingsSupported } from "../../app/findings";
import { FOCUSES, FOCUS_HINT, FOCUS_WORDS, MAX_ISSUES, draftNewTask, lastLimit, rememberLimit, rememberRole } from "../../app/github";
import { LAUNCH_POLICY_FEATURE } from "../../app/launchPolicy";
import { useReviewSupported } from "../../app/review";
import type { AgentChoice } from "../../app/ui";
import { PRODUCT_NAME } from "../../brand";
import { announce } from "../../design/Announcer";
import { Button, IconButton } from "../../design/Button";
import { Chip, Segmented } from "../../design/Chip";
import { TextField } from "../../design/Field";
import { ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { toast } from "../../design/Toast";
import type { FindFocus, Project } from "../../protocol/types";
import { NotAllowed, useMay } from "../../shell/NotAllowed";
import { Screen } from "../../shell/Screen";
import { AgentSheet } from "../talk/AgentSheet";
import { LaunchLine } from "../talk/LaunchLine";
import { agentLabel, createTaskCommand, writerHold, writerHoldText } from "../talk/intent";
import { agentBlocker, agentSent, startOrQueue, asLaunch, draftedWords, useRole, useStartTask } from "../github/common";
import type { RouteProps } from "../types";
import "../settings/settings.css";
import "../github/github.css";

export default function NewFindings({ route }: RouteProps<"findings_new">) {
  const { profile } = useApp();
  const project = useHost((s) => s.projects[route.projectId] ?? null);
  const allowed = useMay("tasks");
  const supported = useFindingsSupported();
  return (
    <Screen title={route.fix ? "Review and fix" : "Review only"} subtitle={project ? `${project.name} · without GitHub` : profile.name}
      className="settings-screen github-screen">
      <div className="settings-stack">
        {!project ? <Notice tone="unknown">This project isn’t on {profile.name} any more.</Notice>
          : !allowed ? <NotAllowed what="start reviews" permission="tasks" />
            : !supported ? <Notice tone="warn" title="Update the computer">{profile.name} can’t keep findings in a file yet. Update {PRODUCT_NAME} on it to use Find & fix without GitHub.</Notice>
              : <NewFindingsForm project={project} fix={route.fix} />}
      </div>
    </Screen>
  );
}

type Where = FindWhere["scope"];
const WHERE_WORDS: Record<Where, string> = { project: "Whole project", folder: "A folder", changes: "Recent changes" };
type Then = "auto_start" | "ask_before_start";
const THEN_WORDS: Record<Then, string> = { auto_start: "Fix by itself", ask_before_start: "Ask first" };
const badFolder = (path: string) => !path.trim() ? "Name a folder in the project" : /^\/|(^|\/)\.\.(\/|$)/.test(path.trim()) ? "A folder inside the project" : null;
/** What a review writes when nobody has chosen. */
const DEFAULT_MOST = 10;
const REVIEWING = "Starting a review";
const QUEUEING = "Adding the fix to Next up";

function NewFindingsForm({ project, fix }: { project: Project; fix: boolean }) {
  const { profile, ui } = useApp();
  const online = useLink().state === "online";
  const launchSupported = useHost((s) => !!s.capabilities?.features.includes(LAUNCH_POLICY_FEATURE));
  const paused = useHost((s) => !!s.queues[project.project_id]?.paused);
  const harnesses = useHost((s) => s.harnesses);
  const writer = useHost((s) => writerHold(s.projects[project.project_id]?.active_writer, s), shallowEqual);
  const { sending, start } = useStartTask();
  // A computer that checks findings gives the review the project's review rules and checks what it writes.
  const checked = useReviewSupported();
  const rememberedFinder = useRole(project, "find");
  const rememberedFixer = useRole(project, "fix");
  const [focus, setFocus] = useState<FindFocus | null>(null);
  const [where, setWhere] = useState<Where>("project");
  const [folder, setFolder] = useState("");
  const [finderChosen, setFinder] = useState<AgentChoice | null>(null);
  const [fixerChosen, setFixer] = useState<AgentChoice | null>(null);
  const [choosing, setChoosing] = useState<"find" | "fix" | null>(null);
  const [most, setMostNow] = useState(() => lastLimit(profile.profileId, project.project_id) ?? DEFAULT_MOST);
  const setMost = (n: number) => { setMostNow(n); rememberLimit(profile.profileId, project.project_id, n); };
  const [then, setThen] = useState<Then>("auto_start");
  // The review started but its fix didn't queue: trying again queues only the fix.
  const [reviewed, setReviewed] = useState(false);
  const mostId = useId();
  const finder = finderChosen ?? rememberedFinder;
  const fixer = fixerChosen ?? rememberedFixer;
  const git = project.vcs?.kind === "git";
  const wheres: Where[] = git ? ["project", "folder", "changes"] : ["project", "folder"];
  const folderError = where === "folder" ? badFolder(folder) : null;
  const blocker = (reviewed ? null : agentBlocker(harnesses, finder, "work")) ?? (fix ? agentBlocker(harnesses, fixer, "work") : null);
  const scope: FindWhere = where === "folder" ? { scope: "folder", path: folder.trim() } : { scope: where };
  const prompt = reviewPrompt({ focus, where: scope, most, checked });
  const title = reviewTitle(focus, scope);
  const waits = writer !== null;

  const queueFix = async (): Promise<boolean> => {
    if (!fixer) return false;
    const agent = agentSent(fixer, launchSupported);
    const ok = await start(createTaskCommand({
      projectId: project.project_id, prompt: fixAllPrompt({ checked }), title: fixAllTitle(), agent, mode: "work",
      placement: { mode: "queue", position: null, authorization: then }, mission: null, bundle: null, github: null,
    }), QUEUEING, then === "auto_start" ? "The fix is in Next up and starts after the review" : "The fix is in Next up and waits for you");
    if (ok) rememberRole(profile.profileId, project.project_id, "fix", asLaunch(agent));
    return ok;
  };
  const begin = async () => {
    if (!reviewed) {
      if (!finder) return;
      const agent = agentSent(finder, launchSupported);
      const placement = startOrQueue(waits);
      const ok = await start(createTaskCommand({
        projectId: project.project_id, prompt, title, agent, mode: "work", placement, mission: null, bundle: null, github: null,
        ...(checked ? { review: "find" as const } : {}),
      }), REVIEWING, waits ? "The review starts when the project is free" : "Review started");
      if (!ok) return;
      rememberRole(profile.profileId, project.project_id, "find", asLaunch(agent));
      if (!fix) { ui.back(); return; }
      setReviewed(true);
    }
    if (await queueFix()) ui.back();
  };
  const edit = () => {
    try {
      announce(draftedWords(draftNewTask(ui, project.project_id, { text: prompt, title, mode: "work", agent: finder })));
    } catch (e) { toast(e instanceof Error ? e.message : describeError(e, profile.name), { tone: "stop" }); }
  };
  const label = reviewed ? "Add the fix to Next up" : waits ? "Start review when free" : "Start review";
  return <>
    {reviewed && (
      <Notice tone="info" title="The review started">Its fix didn’t get into Next up. Try again to add it; the review won’t start twice.</Notice>
    )}
    <Section title="Reviewer">
      <div className="settings-stack">
        <div className="github-focus" role="group" aria-label="Reviewers">
          <Chip selected={focus === null} onClick={() => setFocus(null)}>Default</Chip>
          {FOCUSES.map((f) => <Chip key={f} selected={focus === f} onClick={() => setFocus(f)}>{FOCUS_WORDS[f]}</Chip>)}
        </div>
        <p className="settings-meta">{FOCUS_HINT[focus ?? "default"]}</p>
      </div>
    </Section>
    <Section title="What it reviews">
      <div className="settings-stack">
        <Segmented label="What it reviews" value={where} onChange={setWhere}
          options={wheres.map((w) => ({ value: w, label: WHERE_WORDS[w] }))} />
        {where === "folder" && (
          <TextField label="Folder" value={folder} onChange={setFolder} placeholder="src/payments" autoComplete="off" autoCapitalize="off"
            spellCheck={false} error={folder ? folderError : null} />
        )}
        {where === "changes" && <p className="settings-meta">What the current branch changes from the default branch, or the last 20 commits on the default branch.</p>}
      </div>
    </Section>
    <Section title="Findings">
      <div className="settings-stack">
        <div className="github-stepper" role="group" aria-labelledby={mostId}>
          <span id={mostId} className="github-stepper__label">Most to write</span>
          <IconButton icon={Minus} label="One fewer" disabled={most <= 1} onClick={() => setMost(most - 1)} />
          <output className="github-stepper__value num" aria-live="polite">{most}</output>
          <IconButton icon={Plus} label="One more" disabled={most >= MAX_ISSUES} onClick={() => setMost(most + 1)} />
        </div>
        <p className="settings-meta">
          {`It writes up to ${most}, most serious first, in ${FINDINGS_FILE} at the project’s root, adding to what’s there, and changes no other file. `}
          {checked ? "A second agent then checks each one and sets aside any it can’t show is real. " : ""}
          {fix ? "" : "Fix them from Find & fix, all at once or one at a time."}
        </p>
      </div>
    </Section>
    <Section title="Agents">
      <div className="group">
        <ListRow icon={Search} title="Reviews" chevron disabled={!finder || reviewed} onClick={() => setChoosing("find")}
          subtitle={finder ? `${agentLabel(harnesses, finder)} · writes only ${FINDINGS_FILE}` : "No agent can start work yet"} />
        {fix && (
          <ListRow icon={Wrench} title="Fixes" chevron disabled={!fixer} onClick={() => setChoosing("fix")}
            subtitle={fixer ? agentLabel(harnesses, fixer) : "No agent can start work yet"} />
        )}
      </div>
      {finder && !reviewed && <LaunchLine className="settings-meta" harness={finder.harness} policy={launchSupported ? finder.launchPolicy : null} mode="work" />}
    </Section>
    {fix && (
      <Section title="After the review">
        <div className="settings-stack">
          <Segmented label="After the review" value={then} onChange={setThen}
            options={(["auto_start", "ask_before_start"] as const).map((v) => ({ value: v, label: THEN_WORDS[v] }))} />
          <p className="settings-meta">
            {then === "auto_start"
              ? `The fix waits in Next up and starts once the review is done. It checks each finding is real, fixes and tests it, then ticks it.${checked ? " It fixes only the ones the second agent confirmed." : ""}`
              : "The fix waits in Next up until you start it, so you can read the findings first."}
            {` When every box is ticked, it asks before deleting ${FINDINGS_FILE}.`}
            {paused && " Next up is paused, so nothing in it starts until you resume it."}
          </p>
        </div>
      </Section>
    )}
    {blocker && <p className="settings-meta" role="status">{blocker}</p>}
    {!blocker && waits && writer && !reviewed && <p className="settings-meta" role="status">{writerHoldText(writer, project.name)} The review starts then.</p>}
    <div className="github-list__actions">
      <Button variant="primary" icon={reviewed ? Wrench : Search} disabled={!online || !!blocker || !!folderError || !!sending}
        pending={!!sending} onClick={() => void begin()}>{label}</Button>
      {!fix && <Button variant="quiet" disabled={!!folderError || !finder} onClick={edit}>Edit first</Button>}
    </div>
    {finder && (
      <AgentSheet open={choosing === "find"} onClose={() => setChoosing(null)} project={project} value={finder} mode="work"
        launchFlags={launchSupported} onChange={setFinder} />
    )}
    {fixer && (
      <AgentSheet open={choosing === "fix"} onClose={() => setChoosing(null)} project={project} value={fixer} mode="work"
        launchFlags={launchSupported} onChange={setFixer} />
    )}
  </>;
}
