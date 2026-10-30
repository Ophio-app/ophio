// SPDX-License-Identifier: Apache-2.0
//
// Find & fix without GitHub: a project's FINDINGS.md. A review writes what
// it finds there; where the computer checks findings, a second agent then
// confirms each one or sets it aside with a reason. A fixer works through
// all that wasn't set aside, or one finding at a time, ticking each box,
// then asks before deleting the file. The list here is the file as the
// computer last read it.

import { useState } from "react";
import { Bot, CheckCircle2, CircleDashed, CircleSlash, FileSearch, RefreshCw, Search, ShieldCheck, Wrench } from "lucide-react";
import { describeError, shallowEqual, useApp, useHost, useLink } from "../../app/context";
import { FINDINGS_FILE, type Finding, fixAllPrompt, fixAllTitle, fixOnePrompt, fixOneTitle, isSetAside, useFindingsCheck } from "../../app/findings";
import { useFindings } from "../../app/findingsFile";
import { draftNewTask, rememberRole } from "../../app/github";
import { LAUNCH_POLICY_FEATURE } from "../../app/launchPolicy";
import { SET_ASIDE_WORDS, cap, useReviewSupported, verdictWords } from "../../app/review";
import type { AgentChoice } from "../../app/ui";
import { PRODUCT_NAME } from "../../brand";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { Pill, type Tone } from "../../design/Chip";
import { TextArea } from "../../design/Field";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { Markdown } from "../../design/Markdown";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import type { Project, TaskPlacement } from "../../protocol/types";
import { NotAllowed, useMay } from "../../shell/NotAllowed";
import { Screen } from "../../shell/Screen";
import { AgentSheet } from "../talk/AgentSheet";
import { LaunchLine } from "../talk/LaunchLine";
import { agentLabel, createTaskCommand, writerHold, writerHoldText } from "../talk/intent";
import { agentSent, draftedWords, agentBlocker, nextUp, startOrQueue, asLaunch, useRole, useStartTask, withDirection } from "../github/common";
import type { RouteProps } from "../types";
import "../settings/settings.css";
import "../github/github.css";

export default function Findings({ route }: RouteProps<"findings">) {
  const { profile } = useApp();
  const project = useHost((s) => s.projects[route.projectId] ?? null);
  const allowed = useMay("tasks");
  return (
    <Screen title="Find & fix" subtitle={project ? `${project.name} · without GitHub` : profile.name} className="settings-screen github-screen">
      <div className="settings-stack">
        {!project ? <Notice tone="unknown">This project isn’t on {profile.name} any more.</Notice>
          : !allowed ? <NotAllowed what="start reviews and fixes" permission="tasks" />
            : <FindingsView project={project} />}
      </div>
    </Screen>
  );
}

const SEVERITY_TONE: Record<NonNullable<Finding["severity"]>, Tone> = { high: "stop", medium: "warn", low: "neutral" };
const SEVERITY_WORDS: Record<NonNullable<Finding["severity"]>, string> = { high: "High", medium: "Medium", low: "Low" };

/**
 * What the second agent made of a finding, in a line: "Confirmed — shown by
 * running it", "Set aside — why". One it hasn't judged says "Not checked
 * yet" only where checking is expected, so older files don't promise a check.
 */
function verdictLine(f: Finding, expectChecks: boolean): string | null {
  if (!f.verdict) return expectChecks ? "Not checked yet" : null;
  return verdictWords(f.verdict, f.evidence, isSetAside(f) ? f.reason : null);
}

function FindingsView({ project }: { project: Project }) {
  const { profile, ui } = useApp();
  const online = useLink().state === "online";
  const { text, findings, error, loading, supported, reload } = useFindings(project.project_id);
  const check = useFindingsCheck(project.project_id);
  const reviewSupported = useReviewSupported();
  const [fixing, setFixing] = useState<Finding | "all" | null>(null);
  const [shown, setShown] = useState<number | null>(null);
  const open = findings?.filter((f) => !f.done && !isSetAside(f)) ?? [];
  const aside = findings?.filter((f) => !f.done && isSetAside(f)) ?? [];
  const done = findings?.filter((f) => f.done) ?? [];
  // "Not checked yet" only where a check is expected: this computer checks findings, or this file has been checked.
  const expectChecks = reviewSupported || !!findings?.some((f) => f.verdict);
  const checking = check === "checking";
  const pid = project.project_id;
  const finding = shown === null ? undefined : findings?.find((f) => f.number === shown);
  if (!supported) {
    return <Notice tone="warn" title="Update the computer">{profile.name} can’t keep findings in a file yet. Update {PRODUCT_NAME} on it to use Find & fix without GitHub.</Notice>;
  }
  const row = (f: Finding) => {
    const verdict = f.done ? null : verdictLine(f, expectChecks);
    return (
      <ListRow key={f.number} icon={f.done ? CheckCircle2 : f.verdict === "confirmed" ? ShieldCheck : CircleDashed} title={`${f.number}. ${f.title}`} chevron onClick={() => setShown(f.number)}
        subtitle={f.done ? f.note ?? "Ticked" : <>{f.where}{verdict && <span className="github-item__note">{verdict}</span>}</>}
        meta={!f.done && f.severity && <Pill tone={SEVERITY_TONE[f.severity]}>{SEVERITY_WORDS[f.severity]}</Pill>} />
    );
  };
  // A set-aside finding leads with why: that is what the person weighs.
  const asideRow = (f: Finding) => (
    <ListRow key={f.number} icon={CircleSlash} title={`${f.number}. ${f.title}`} chevron onClick={() => setShown(f.number)}
      subtitle={<>{f.reason ? cap(f.reason) : "The second agent gave no reason."}{f.where && <span className="github-item__note">{f.where}</span>}</>}
      meta={<Pill tone="neutral">{SET_ASIDE_WORDS[f.verdict as "rejected" | "uncertain"]}</Pill>} />
  );
  return <>
    <p className="settings-meta">A review writes what it finds in {FINDINGS_FILE} at the project’s root, and a fixer ticks each one off. Nothing goes to GitHub.</p>
    <div className="group">
      <ListRow icon={Search} title="Review and fix" chevron subtitle="An agent writes what it finds, then another fixes each one"
        onClick={() => ui.push({ screen: "findings_new", projectId: pid, fix: true })} />
      <ListRow icon={FileSearch} title="Review only" chevron subtitle={`An agent writes what it finds in ${FINDINGS_FILE} and changes nothing else`}
        onClick={() => ui.push({ screen: "findings_new", projectId: pid, fix: false })} />
      <ListRow icon={Wrench} title="Fix all" chevron disabled={!open.length}
        subtitle={!open.length ? "Nothing open to fix"
          : checking ? "Waits until the second agent has checked them"
            : `An agent fixes the ${open.length === 1 ? "open finding" : `${open.length} open findings`}, in order${aside.length ? ", and leaves the set-aside ones" : ""}`}
        onClick={() => setFixing("all")} />
    </div>
    {checking && (
      <Notice tone="info" report={false} icon={ShieldCheck} title="A second agent is checking the findings">
        It tries to show each one is real before anything is fixed, and sets aside any it can’t. Fix all waits until it’s done.
      </Notice>
    )}
    {check === "stopped" && (
      <Notice tone="unknown" report={false} title="The check stopped before it finished">
        The findings it didn’t reach say Not checked yet. Fix all still fixes them; read them first if you want to be sure.
      </Notice>
    )}
    {error && <Notice tone="unknown" action={<Button size="sm" disabled={!online} onClick={reload}>Try again</Button>}>{error}</Notice>}
    {text === undefined && !error && (online
      ? <div className="screen__loading" aria-busy="true" aria-label={`Reading ${FINDINGS_FILE}`} />
      : <Notice tone="unknown">Connect to {profile.name} to read {FINDINGS_FILE}.</Notice>)}
    {text === null && <Empty icon={FileSearch} title={`No ${FINDINGS_FILE} yet`}>Start a review and what it finds shows here. Fix them all at once, or one at a time.</Empty>}
    {findings && (findings.length === 0
      ? <Notice tone="info">{FINDINGS_FILE} has nothing in the form a review writes, so there’s nothing to list.</Notice>
      : <>
        {open.length === 0 && aside.length === 0 && <Notice tone="info" icon={CheckCircle2} title="Every finding is ticked">The fixer asks before it deletes {FINDINGS_FILE}. Answer it in its task.</Notice>}
        {open.length === 0 && aside.length > 0 && <Notice tone="info" report={false} icon={CheckCircle2} title="Nothing left to fix">What’s left was set aside by the second agent. Open one to read why, and fix it anyway if you think it’s real.</Notice>}
        {open.length > 0 && <Section title="Open"><div className="group">{open.map(row)}</div></Section>}
        {aside.length > 0 && (
          <Section title="Set aside">
            <p className="settings-meta">A second agent checked these and couldn’t show they’re real problems. They stay in {FINDINGS_FILE}, and Fix all leaves them alone.</p>
            <div className="group">{aside.map(asideRow)}</div>
          </Section>
        )}
        {done.length > 0 && <Section title="Done"><div className="group">{done.map(row)}</div></Section>}
      </>)}
    {text !== undefined && (
      <div className="github-list__actions">
        <Button variant="quiet" size="sm" icon={RefreshCw} disabled={!online || loading} pending={loading} onClick={reload}>Read it again</Button>
      </div>
    )}
    {finding && (
      <FindingSheet finding={finding} expectChecks={expectChecks} onClose={() => setShown(null)} onFix={() => { setShown(null); setFixing(finding); }} />
    )}
    {fixing && <FixSheet project={project} finding={fixing === "all" ? null : fixing} checking={fixing === "all" && checking}
      skips={fixing === "all" ? aside.length : 0} onClose={() => setFixing(null)} />}
  </>;
}

/** What the second agent's reason is, by its verdict. */
const REASON_TITLE: Record<NonNullable<Finding["verdict"]>, string> = {
  confirmed: "How it was shown", rejected: "Why it isn’t a problem", uncertain: "What couldn’t be settled",
};

/** One finding as the file has it, with what the second agent made of it and what the fixer said if it got to it. */
function FindingSheet({ finding, expectChecks, onClose, onFix }: { finding: Finding; expectChecks: boolean; onClose: () => void; onFix: () => void }) {
  // Severity, place and verdict are shown above the text, so they aren't repeated in it.
  const body = finding.details;
  const aside = isSetAside(finding);
  const verdict = verdictLine({ ...finding, reason: null }, expectChecks);
  return (
    <Sheet open onClose={onClose} title={`${finding.number}. ${finding.title}`} subtitle={finding.done ? "Ticked" : aside ? "Set aside" : "Open"}
      footer={!finding.done && <Button variant={aside ? "secondary" : "primary"} icon={Wrench} onClick={onFix}>{aside ? "Fix it anyway" : "Fix this one"}</Button>}>
      <div className="settings-stack">
        {(finding.severity || finding.where) && (
          <p className="settings-meta">
            {finding.severity && <Pill tone={SEVERITY_TONE[finding.severity]}>{SEVERITY_WORDS[finding.severity]}</Pill>}
            {finding.where && <> {finding.where}</>}
          </p>
        )}
        {verdict && (
          <Notice tone={finding.verdict === "confirmed" ? "info" : "unknown"} report={false} icon={finding.verdict === "confirmed" ? ShieldCheck : aside ? CircleSlash : CircleDashed}
            title={aside ? `Set aside: ${SET_ASIDE_WORDS[finding.verdict as "rejected" | "uncertain"].toLowerCase()}` : verdict}>
            {finding.verdict && finding.reason ? <><strong>{REASON_TITLE[finding.verdict]}:</strong> {finding.reason}</>
              : finding.verdict ? "The second agent gave no more detail."
                : "A second agent hasn’t checked this one. It may still be a real problem, or not one at all."}
            {aside && !finding.done && " Fix all leaves it alone. You can still have it fixed if you think it’s real."}
          </Notice>
        )}
        {body ? <Markdown text={body} /> : <p className="settings-meta">The review wrote nothing more about it.</p>}
      </div>
    </Sheet>
  );
}

const STARTING = "Starting a fix";
const QUEUEING = "Adding a fix to Next up";

/** Who fixes, anything else to tell them, and starting now, when the project is free, or later. */
function FixSheet({ project, finding, checking, skips, onClose }: {
  project: Project; finding: Finding | null;
  /** A second agent is still checking the findings Fix all would fix. */
  checking: boolean;
  /** Findings Fix all leaves because they were set aside. */
  skips: number;
  onClose: () => void;
}) {
  const { profile, ui } = useApp();
  const online = useLink().state === "online";
  const harnesses = useHost((s) => s.harnesses);
  const launchSupported = useHost((s) => !!s.capabilities?.features.includes(LAUNCH_POLICY_FEATURE));
  const writer = useHost((s) => writerHold(s.projects[project.project_id]?.active_writer, s), shallowEqual);
  const remembered = useRole(project, "fix");
  const [chosen, setChosen] = useState<AgentChoice | null>(null);
  const [choosing, setChoosing] = useState(false);
  const [direction, setDirection] = useState("");
  const { sending, start } = useStartTask();
  const agent = chosen ?? remembered;
  const waits = writer !== null;
  const blocker = agentBlocker(harnesses, agent, "work");
  const prompt = withDirection(finding ? fixOnePrompt(finding) : fixAllPrompt(), direction);
  const title = finding ? fixOneTitle(finding) : fixAllTitle();
  const what = finding ? `Finding ${finding.number}` : "The fix";
  const send = async (placement: TaskPlacement, label: string, done: string) => {
    if (!agent) return;
    const sent = agentSent(agent, launchSupported);
    const body = createTaskCommand({ projectId: project.project_id, prompt, title, agent: sent, mode: "work", placement, mission: null, bundle: null, github: null });
    if (await start(body, label, done)) {
      rememberRole(profile.profileId, project.project_id, "fix", asLaunch(sent));
      onClose();
    }
  };
  const edit = () => {
    try {
      announce(draftedWords(draftNewTask(ui, project.project_id, { text: prompt, title, mode: "work", agent })));
      onClose();
    } catch (e) { toast(e instanceof Error ? e.message : describeError(e, profile.name), { tone: "stop" }); }
  };
  const disabled = !online || !!blocker || !!sending;
  const now = startOrQueue(waits);
  return (
    <Sheet open onClose={onClose} title={finding ? `Fix ${finding.number}. ${finding.title}` : "Fix all"} subtitle={project.name}
      footer={<>
        <Button variant="secondary" disabled={disabled} pending={sending === QUEUEING}
          onClick={() => void send(nextUp(), QUEUEING, `${what} is in Next up`)}>Add to Next up</Button>
        <Button variant="primary" disabled={disabled} pending={sending === STARTING}
          onClick={() => void send(now, STARTING, waits ? `${what} starts when the project is free` : `Started on ${finding ? `finding ${finding.number}` : "the fixes"}`)}>
          {waits ? "Start when free" : checking ? "Start anyway" : "Start"}
        </Button>
      </>}>
      <div className="settings-stack">
        {checking && (
          <Notice tone="warn" report={false} title="The findings are still being checked">
            A second agent is checking each one. If you start now, the fixer may work on findings it would have set aside. Wait for it to finish, or start anyway.
          </Notice>
        )}
        <div className="group">
          <ListRow icon={Bot} title="Fixed by" subtitle={agent ? agentLabel(harnesses, agent) : "No agent can start work yet"} chevron
            onClick={() => setChoosing(true)} disabled={!agent} />
        </div>
        {agent && <LaunchLine className="settings-meta" harness={agent.harness} policy={launchSupported ? agent.launchPolicy : null} mode="work" />}
        <TextArea label="More direction" value={direction} onChange={setDirection} rows={2} maxRows={6}
          hint="Optional. Anything else the fixer should know." />
        {blocker ? <p className="settings-meta" role="status">{blocker}</p> : <>
          <p className="settings-meta">
            {finding ? `It checks finding ${finding.number} is real, fixes and tests it, then ticks it.` : "It works through each open finding in order: checks it’s real, fixes and tests it, then ticks it."}
            {skips > 0 && ` It leaves the ${skips === 1 ? "one" : skips} set aside alone.`}
            {` Once nothing is left open, it asks before deleting ${FINDINGS_FILE}.`}
          </p>
          {waits && writer && <p className="settings-meta" role="status">{writerHoldText(writer, project.name)} Start when free starts it then.</p>}
        </>}
        <div className="github-list__actions">
          <Button variant="quiet" size="sm" disabled={!agent} onClick={edit}>Edit first</Button>
        </div>
      </div>
      {agent && <AgentSheet open={choosing} onClose={() => setChoosing(false)} project={project} value={agent} mode="work" launchFlags={launchSupported} onChange={setChosen} />}
    </Sheet>
  );
}
