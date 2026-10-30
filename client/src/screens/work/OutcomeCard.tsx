// SPDX-License-Identifier: Apache-2.0
//
// The top of a receipt: what the latest run came to and what to do about it.
// What it asks or left undone comes first with the way to answer or carry on;
// separate work it found is offered to start as tasks of their own, never
// started by the agent itself, with any change of mode said right above its
// Start; once it is done the task can be archived, alone or to start a new
// one. A button
// that says what it sends sends it: Start and Carry out this plan go through
// Review, which stays open only when something there needs choosing.
// It heads a receipt and ends the conversation in Talk, where the message
// below is always open. What the run showed the person, its pictures,
// previews and asks, is here with what it came to, however far up the
// conversation it was shown.

import { useEffect, useId, useMemo, useState } from "react";
import { Archive, CircleDot, FileDiff, GitPullRequest, ListChecks, MessageSquare, Pencil, Play } from "lucide-react";
import { describeError, shallowEqual, useApp, useHost, useLink, useUi } from "../../app/context";
import { draftNewTask, taskPr, useGithubSupported } from "../../app/github";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { Pill } from "../../design/Chip";
import { Notice } from "../../design/Notice";
import { toast } from "../../design/Toast";
import { useNow } from "../../design/time";
import { draftKey } from "../../app/ui";
import type { NewWork, Project, Run, Task } from "../../protocol/types";
import { defaultLaunchPolicy } from "../../app/launchPolicy";
import { reviewPlan, type CarryPlan } from "../talk/carryPlan";
import { ModeSwitch } from "../talk/ModeSwitch";
import { writerHoldState } from "../talk/intent";
import { sendOnOpen, takeSendAsk } from "../talk/sendOnOpen";
import { ShownCard } from "../talk/Shown";
import { useGithubLinkable } from "../github/GithubDoors";
import { PrSheet } from "../github/PrSheet";
import { ReviewResultCard, TaskFindings } from "../github/ReviewResult";
import { useArchive } from "./archive";
import { carryOn, carryOnHold } from "./carryOn";
import { CARRY_ON, TRY_AGAIN, newWorkMeta, reachesRunStart, runShown, type Outcome, type RunShown } from "./outcome";
import { useCanWork } from "./shared";
import { followUpLine, verificationLine } from "./logic";
import "./work.css";

export function OutcomeCard({ task, run, project, outcome, plan, writing, compact, onWrite, onSent, onCorrect, onFollow, onSeeChanges }: {
  task: Task;
  run: Run;
  project: Project;
  outcome: Outcome;
  plan: CarryPlan | null;
  /** Whether the message below is open, so there's no need to offer it. */
  writing: boolean;
  /** Smaller buttons, where it ends a conversation. */
  compact?: boolean;
  /** Opens the message below. */
  onWrite?: () => void;
  /** After Carry on or Try again reached the computer. */
  onSent?: () => void;
  /** Drafts what to fix, on a picture where there is one. */
  onCorrect?: () => void;
  /** Opens the conversation in Talk. */
  onFollow?: () => void;
  /** Shows the changed files where they already are; otherwise the receipt opens at them. */
  onSeeChanges?: () => void;
}) {
  const app = useApp();
  const { ui, profile } = app;
  const heading = useId();
  const can = useCanWork();
  const github = useGithubLinkable(project);
  const archive = useArchive();
  const [error, setError] = useState<string | null>(null);
  const [planning, setPlanning] = useState(false);
  const [carrying, setCarrying] = useState(false);
  const [prOpen, setPrOpen] = useState(false);
  const hold = useHost((s) => carryOnHold(s, task), shallowEqual);
  // A message still on its way, or not known to have arrived, is settled
  // first, so a second tap never sends it twice.
  const inDoubt = useUi((s) => {
    const phase = s.drafts[draftKey(task.project_id, task.task_id)]?.attempt?.phase;
    return phase === "sending" || phase === "unknown";
  });
  const now = useNow();
  const found = task.github?.link.type === "find";
  const ended = outcome.kind !== "waiting";
  const shown = useRunShown(task, run, ended);
  const showing = shown.slice(-SHOWN_IN_CARD);
  const size = compact ? "sm" : undefined;
  const archived = () => toast(`Archived “${task.title}”`);
  const start = (w: NewWork) => {
    setError(null);
    sendOnOpen(task.project_id, null);
    try {
      draftNewTask(ui, task.project_id, {
        text: w.brief || w.title, title: w.title, mode: w.mode, from: task.mode ?? "work",
        // The issue it filed is linked where the project uses GitHub.
        link: w.issue && github ? { type: "issue", number: w.issue } : undefined,
        agent: { harness: run.harness, model: run.requested_model, effort: run.requested_effort },
      });
    } catch (e) {
      takeSendAsk(task.project_id, null);
      setError(describeError(e, profile.name));
    }
  };
  const carry = async (text: string) => {
    setCarrying(true); setError(null);
    try {
      const sent = await carryOn(app, task, text, hold !== null);
      if (sent.ok) onSent?.();
      else {
        setError(sent.attempt.detail ?? "It wasn’t sent.");
        // What happened to it is kept with the message below.
        onWrite?.();
      }
    } finally {
      setCarrying(false);
    }
  };
  const carryButton = (label: string, text: string, variant: "primary" | "secondary" = "primary") => <Button variant={variant} size={size} icon={Play}
    pending={carrying} disabled={!can || inDoubt} onClick={() => void carry(text)}>{label}</Button>;
  const carryOutPlan = async () => {
    setPlanning(true); setError(null);
    try { await reviewPlan(app, task, undefined, { send: true }); } catch (e) { setError(describeError(e, profile.name)); } finally { setPlanning(false); }
  };
  // A finished task leads with what it made: its pull request, or the files
  // it changed. Only that one button is lime; the first new work's Start
  // leads when there's nothing to look at, and Archive when there's neither.
  const pr = taskPr(task.github);
  const prShown = useGithubSupported() && !!pr;
  const changed = run.outcome?.changed_files.length ?? 0;
  const result = outcome.kind === "done" && !plan && (prShown
    ? <Button variant="primary" size={size} icon={GitPullRequest} onClick={() => setPrOpen(true)}>See pull request #{pr!.number}</Button>
    : changed > 0 && <Button variant="primary" size={size} icon={FileDiff}
      onClick={onSeeChanges ?? (() => ui.push({ screen: "receipt", taskId: task.task_id, runId: run.run_id, section: "changes" }))}>
      See changes · {changed} {changed === 1 ? "file" : "files"}</Button>);
  const archiveButtons = ended && <>
    <Button icon={Archive} size={size} variant={outcome.kind === "done" && !plan && !outcome.doubts.length && !outcome.newWork.length && !result ? "primary" : "secondary"}
      disabled={!archive.can} onClick={() => archive.ask([task], archived)}>Archive</Button>
    {outcome.kind === "done" && <Button icon={Pencil} size={size} disabled={!archive.can}
      onClick={() => archive.ask([task], () => ui.openTask(task.project_id, null))}>Archive and start new</Button>}
  </>;
  return (
    <section className={`receipt-outcome receipt-outcome--${outcome.kind}${outcome.doubts.length ? " receipt-outcome--doubt" : ""}`} aria-labelledby={heading}>
      <h2 id={heading} className="receipt-outcome__title">{outcome.title}</h2>
      {outcome.detail && <p className="receipt-outcome__detail">{outcome.detail}</p>}
      {outcome.kind === "done" && run.outcome?.verification?.status !== "failed" && verificationLine(run) && <p className="fine">{verificationLine(run)}</p>}
      {followUpLine(run) && <p className="fine">{followUpLine(run)}</p>}
      {outcome.kind === "watching" && task.wake && <p className="fine">
        Nothing’s needed from you. {task.wake.answer ? "Ophio wakes it when the answer comes" : task.wake.at > now ? <>Ophio wakes it <Age at={task.wake.at} /></> : "Ophio wakes it once the project is free"}.
      </p>}
      {outcome.items.length > 0 && <ul className="receipt-outcome__items">
        {outcome.items.map((item, i) => <li key={i}>{item}</li>)}
      </ul>}
      {/* Its pull request's review, and what a review task's findings came to. */}
      {prShown && <ReviewResultCard task={task} />}
      <TaskFindings review={task.review} />
      {showing.length > 0 && <div className="receipt-outcome__shown">
        <h3>{shown.length === 1 ? "What it showed you" : `What it showed you · ${shown.length}`}</h3>
        {showing.map((r) => <ShownCard key={r.key} shown={r.shown} taskId={task.task_id} at={r.at} />)}
        {shown.length > showing.length && <p className="fine">{shown.length - showing.length === 1
          ? "It showed you one more earlier in the conversation."
          : `It showed you ${shown.length - showing.length} more earlier in the conversation.`}</p>}
      </div>}
      {/* Carrying out a plan is Work: said right above the button. */}
      {outcome.kind === "done" && plan && <ModeSwitch from={task.mode ?? "work"} to="work" who="It"
        flags={task.launch_policy ? { harness: task.harness, launchPolicy: task.launch_policy } : null} />}
      <div className="work-actions">
        {outcome.kind === "waiting" && onFollow && <Button variant="primary" icon={MessageSquare} onClick={onFollow}>Open the conversation</Button>}
        {outcome.kind === "needs_you" && !writing && onWrite && <Button variant="primary" icon={MessageSquare} onClick={onWrite}>Answer</Button>}
        {(outcome.kind === "unfinished" || outcome.kind === "stopped") && carryButton("Carry on", CARRY_ON)}
        {outcome.kind === "watching" && carryButton("Carry on now", CARRY_ON, "secondary")}
        {outcome.kind === "failed" && <>
          {carryButton("Try again", TRY_AGAIN)}
          {onCorrect && <Button onClick={onCorrect}>Say what to fix</Button>}
        </>}
        {/* Continuing is the button below, or the message itself in Talk. */}
        {outcome.kind === "done" && plan && <Button variant="primary" size={size} pending={planning} disabled={!can} onClick={() => void carryOutPlan()}>Carry out this plan</Button>}
        {result}
        {archiveButtons}
      </div>
      {hold && (outcome.kind === "unfinished" || outcome.kind === "stopped" || outcome.kind === "failed" || outcome.kind === "watching") && <p className="fine">
        {outcome.kind === "failed" ? "Try again" : outcome.kind === "watching" ? "Carry on now" : "Carry on"} waits in Next up. {writerHoldState(hold)}.
      </p>}
      {inDoubt && (outcome.kind === "unfinished" || outcome.kind === "stopped" || outcome.kind === "failed" || outcome.kind === "watching") && <p className="fine">
        Check the message below first: it may already have reached {profile.name}.
      </p>}
      {found && <div className="receipt-outcome__found">
        <p>What it found is filed as issues, and fixes are queued where the review asked for them.</p>
        <div className="work-actions">
          <Button icon={CircleDot} onClick={() => ui.push({ screen: "github", projectId: task.project_id, view: "issues" })}>Issues</Button>
          <Button icon={ListChecks} onClick={() => ui.push({ screen: "next_up" }, "work")}>Next up</Button>
        </div>
      </div>}
      {outcome.newWork.length > 0 && <div className="receipt-outcome__new">
        <h3>{outcome.newWork.length === 1 ? "New work it found" : `New work it found · ${outcome.newWork.length}`}</h3>
        {outcome.newWork.map((w, i) => <div className="receipt-new-work work-card" key={i}>
          <div className="receipt-new-work__head">
            <h4>{w.title}</h4>
            <Pill>{newWorkMeta(w)}</Pill>
          </div>
          {w.brief && w.brief !== w.title && <p className="receipt-new-work__brief">{w.brief}</p>}
          <ModeSwitch from={task.mode ?? "work"} to={w.mode} flags={{ harness: run.harness, launchPolicy: defaultLaunchPolicy(run.harness) }} />
          <Button variant={!result && i === 0 ? "primary" : "secondary"} size="sm" icon={Play} onClick={() => start(w)}>Start</Button>
        </div>)}
      </div>}
      {error && <Notice tone="stop">{error}</Notice>}
      {archive.sheet}
      {prShown && <PrSheet open={prOpen} onClose={() => setPrOpen(false)} projectId={task.project_id} number={pr!.number} task={task} />}
    </section>
  );
}

/** How many of the things a run showed its card holds, the latest. */
const SHOWN_IN_CARD = 4;

/** What the run showed the person. Once it ended, earlier pages of the
 * conversation are read until they reach the run's start, so nothing it
 * showed is missing however long it went on after. */
function useRunShown(task: Task, run: Run, ended: boolean): RunShown[] {
  const { session } = useApp();
  const link = useLink();
  const timeline = useHost((s) => s.timelines[task.task_id]);
  const items = timeline?.loaded ? timeline.items : null;
  const loaded = !!items;
  useEffect(() => {
    if (!ended || !loaded || link.state !== "online") return;
    let alive = true;
    void (async () => {
      while (alive) {
        const page = session.state.get().timelines[task.task_id];
        if (!page?.loaded || !page.hasMore || reachesRunStart(page.items, run.run_id)) return;
        await session.loadTimeline(task.task_id, true);
        if (session.state.get().timelines[task.task_id]?.items.length === page.items.length) return;
      }
    })().catch(() => undefined);
    return () => { alive = false; };
  }, [ended, loaded, link.state, session, task.task_id, run.run_id]);
  return useMemo(() => (items ? runShown(items, run.run_id) : []), [items, run.run_id]);
}
