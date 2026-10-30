// SPDX-License-Identifier: Apache-2.0
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { TextArea } from "../../design/Field";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { StateLine } from "../../design/StateLine";
import { announce } from "../../design/Announcer";
import { CommandError, NotConnectedError } from "../../host/session";
import { NotAllowed } from "../../shell/NotAllowed";
import { uuidv7 } from "../../protocol/ids";
import type { ProjectStage, ProjectStageHistory, ProjectStageTransition } from "../../protocol/types";
import { useStore } from "../../state/store";
import { StageDraftStore, stageName, stageRevision, staleStage } from "./stages";

interface Review { target: ProjectStage; note: string; revision: number; from: ProjectStage | null }
export function StageChange({ projectId, name, current, history, refresh, loading, available, feedbackTarget }: {
  feedbackTarget: HTMLDivElement | null;
  projectId: string; name: string; current: ProjectStageTransition | null;
  history: ProjectStageHistory | undefined; refresh: () => void; loading: boolean; available: boolean;
}) {
  const { session, profile } = useApp();
  const reasonId = useId();
  const body = useRef<HTMLDivElement>(null);
  const link = useLink();
  const online = link.state === "online";
  const admin = useHost(s => s.grants?.admin === true);
  const [store] = useState(() => new StageDraftStore(`${profile.profileId}.stages.${projectId}.draft`));
  const draft = useStore(store, s => s);
  const [open, setOpen] = useState(false);
  const [review, setReview] = useState<Review | null>(null);
  const [checking, setChecking] = useState(false);
  const [checkVersion, setCheckVersion] = useState(0);
  const [checkError, setCheckError] = useState<string | null>(null);
  const attempt = draft.attempt;
  const live = useStore(session.commands, all => attempt ? all[attempt.id] : undefined);
  const pending = attempt?.phase === "sending" || (attempt?.phase === "unknown" && live?.phase === "sending");
  const unresolved = pending || attempt?.phase === "unknown";
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const selected = history?.stages.find(s => s === draft.target);
  const limit = history?.note_limit;
  const tooLong = limit !== undefined && [...draft.note].length > limit;
  const permission = <NotAllowed what="change the project stage" permission="manage" />;
  const reason = link.state === "failed"
    ? link.reason === "revoked" ? "This device was removed. Pair it again to change the stage." : "Resolve the connection problem before changing the stage."
    : !online ? "Reconnect to change the stage." : !admin ? "Project settings permission is required to change the stage." : !available ? "Load the current stage history before making a change." : null;

  useLayoutEffect(() => {
    if (!open || !body.current) return;
    const heading = body.current.closest("dialog")?.querySelector("h2");
    heading?.focus({ preventScroll: true });
    if (body.current.parentElement) body.current.parentElement.scrollTop = 0;
  }, [open, review]);

  useEffect(() => {
    if (!live?.receipt || !attempt || !["sending", "unknown"].includes(attempt.phase)) return;
    store.receipt(live.receipt);
  }, [live?.receipt, attempt, store]);
  useEffect(() => {
    if (attempt?.phase !== "recorded") return;
    setOpen(false); setReview(null);
    refreshRef.current();
    announce(attempt.message);
  }, [attempt]);
  // A send interrupted by app death is checked using its saved id. Merely
  // reopening the page never sends or repeats the stage command.
  useEffect(() => {
    if (!online || attempt?.phase !== "unknown" || live?.phase === "sending") {
      setChecking(false);
      return;
    }
    let alive = true;
    setChecking(true);
    setCheckError(null);
    void session.query({ kind: "command_status", command_id: attempt.id }).then(r => {
      if (!alive) return;
      if (r.receipt) store.receipt(r.receipt);
      else store.update({ attempt: { ...attempt, phase: "not_sent", message: "The computer has no record of this request. Review again to send it." } });
      refreshRef.current();
    }).catch(e => { if (alive) setCheckError(describeError(e, profile.name)); })
      .finally(() => { if (alive) setChecking(false); });
    return () => { alive = false; };
  }, [online, attempt?.id, attempt?.phase, live?.phase, session, store, profile.name, checkVersion]);

  async function confirm() {
    if (!review || reason || unresolved) return;
    const id = uuidv7();
    if (!store.update({ attempt: { id, phase: "sending", message: "Waiting for the computer to record the stage." } })) {
      store.update({ attempt: { id, phase: "not_sent", message: "Not sent. The draft couldn’t be saved on this device." } });
      return;
    }
    try {
      const receipt = await session.command({ command: "set_project_stage", project_id: projectId, stage: review.target, note: review.note.trim() || null },
        { label: `Set ${name} stage`, expectedRevision: review.revision, commandId: id });
      store.receipt(receipt);
    } catch (e) {
      const message = e instanceof CommandError && e.error.code === "stale_revision" ? staleStage(e.error) : describeError(e, profile.name);
      store.update({ attempt: { id, phase: e instanceof CommandError ? "refused" : e instanceof NotConnectedError ? "not_sent" : "unknown", message } });
      setReview(null);
      refreshRef.current();
      announce(message);
    }
  }
  const status = attempt && <div className="work-stack">
    <StateLine phase={pending ? "sending" : attempt.phase === "unknown" ? "unknown" : attempt.phase === "recorded" ? "saved_on_computer" : attempt.phase === "refused" ? "rejected" : "not_delivered"} computer={profile.name} detail={attempt.message} />
    {checkError && <Notice tone="unknown">{checkError}</Notice>}
    {attempt.phase === "unknown" && <Button pending={checking} disabled={!online || pending} onClick={() => setCheckVersion(n => n + 1)}>Check result</Button>}
  </div>;
  return <>
    {feedbackTarget && createPortal(<div className="work-stack">
      {!open && status}
      {!open && !draft.saved && <Notice tone="stop">This device couldn’t save the draft. Keep this screen open and free some storage before sending.</Notice>}
      {reason && <div id={reasonId}>{!admin && online ? permission : <p className="work-muted">{reason}</p>}</div>}
    </div>, feedbackTarget)}
    <div className="stages-actions">
      <Button variant="primary" block aria-describedby={reason ? reasonId : undefined} disabled={!!reason || unresolved || loading} onClick={() => { setReview(null); setOpen(true); }}>Change stage</Button>
    </div>
    <Sheet open={open} onClose={() => setOpen(false)} className="work-sheet stages-sheet" title={review ? "Review stage" : "Change stage"} subtitle={name}
      footer={<div className="stages-footer">
        {review ? <Button variant="primary" block pending={pending} disabled={!!reason || unresolved} onClick={() => void confirm()}>{pending ? "Recording…" : "Confirm stage change"}</Button>
          : <Button variant="primary" block disabled={!!reason || unresolved || !selected || selected === current?.to || tooLong || loading} onClick={() => {
            if (selected) setReview({ target: selected, note: draft.note, revision: stageRevision(current), from: current?.to ?? null });
          }}>Review stage</Button>}
      </div>}>
      <div className="work-stack" ref={body}>
        {reason && (!admin && online ? permission : <Notice>{reason}</Notice>)}
        {!draft.saved && <Notice tone="stop">This device couldn’t save the draft. Keep this screen open and free some storage before sending.</Notice>}
        {status}
        {review ? <>
          <p className="stages-transition">{stageName(review.from)} <span aria-hidden>→</span><span className="visually-hidden">to</span> <strong>{stageName(review.target)}</strong></p>
          <Notice title="Stage decision only">No agent gets new permissions. No work starts, and nothing is released or published.</Notice>
          <p>Adds this decision and your device{review.note.trim() ? ", along with your note," : ""} to the project’s history.</p>
          {review.note.trim() && <div className="stages-note"><p className="work-caption">Your note</p><p>{review.note}</p></div>}
          <Button variant="quiet" disabled={unresolved} onClick={() => setReview(null)}>Back to choices</Button>
        </> : <>
          <p className="work-muted">Current: {stageName(current?.to)}. Moving back to an earlier stage is recorded too.</p>
          <fieldset className="stages-choices" disabled={unresolved}>
            <legend>Choose a stage</legend>
            {history?.stages.map(stage => <label key={stage} className="stages-choice">
              <input type="radio" name="project-stage" value={stage} checked={draft.target === stage} onChange={() => store.update({ target: stage })} />
              <span>{stageName(stage)}{stage === current?.to && <span className="work-caption"> · Current</span>}</span>
            </label>)}
          </fieldset>
          <TextArea label="Note (optional)" rows={2} maxRows={5} value={draft.note} disabled={unresolved} onChange={note => store.update({ note })} hint={limit ? `Up to ${limit} characters. ${draft.saved ? "Saved on this device until confirmed." : "Not saved on this device."}` : undefined} />
          {tooLong && <Notice tone="warn">Shorten the note to {limit} characters before reviewing.</Notice>}
        </>}
      </div>
    </Sheet>
  </>;
}
