// SPDX-License-Identifier: Apache-2.0
import { Bot, Pencil } from "lucide-react";
import { useState } from "react";
import { describeError, useApp, useCommand, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { holdsHumanInput, releasedTakeover } from "../control/controlStatus";
import { handBackTarget } from "../../state/handBack";
import { annotationStore } from "../../state/annotations";
import { Sheet } from "../../design/Sheet";
import { useNow } from "../../design/time";
import type { Task } from "../../protocol/types";
import { LivePicture } from "../control/LivePicture";
import { frameFresh } from "../control/lease";
import { PendingMessageError } from "../control/previewAnnotation";
import { inputBlocker } from "../control/readiness";
import { Unlock } from "../control/Unlock";
import { useMedia } from "../control/useMedia";
import { captureSource } from "../annotate/sources";
import { composerMode, defaultIntent } from "./intent";
import { returnToAgent } from "./returnToAgent";
import { draftKey } from "../../app/ui";

/** The agent acts on the screen only while someone watches it, so a hand-back
 * from anywhere but that screen goes on to it; `watching` where it's shown. */
export function HandBack({ task, reviewOnly = false, watching = false, onClose }: { task: Task; reviewOnly?: boolean; watching?: boolean; onClose?: () => void }) {
  const { session, profile, platform, ui } = useApp();
  const execute = useCommand();
  const [review, setReview] = useState(reviewOnly);
  const [busy, setBusy] = useState(false);
  const [capturing, setCapturing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const online = useLink().state === "online";
  const now = useNow();
  const target = useHost(s => s.targets.find(t => t.target_id === task.target_id));
  const view = useHost(s => s.control[task.target_id ?? ""]);
  const run = useHost(s => s.runs[task.current_run ?? ""]);
  const caps = useHost(s => s.capabilities);
  const harness = useHost(s => s.harnesses.find(h => h.kind === task.harness));
  const grants = useHost(s => s.grants);
  const caughtUp = useHost(s => s.caughtUp);
  const mine = holdsHumanInput(view, profile.deviceId);
  const waitingTarget = useHost(s => handBackTarget(s, task));
  const takenOver = mine || !!waitingTarget;
  const close = () => { setReview(false); onClose?.(); };
  const reason = inputBlocker(caps, target, view, grants);
  const media = useMedia(target, review && online && !!grants?.view && !reason);
  const live = run && ["working", "awaiting_input", "awaiting_approval"].includes(run.state) && !run.ended_at;
  const fresh = media.picture.connected && frameFresh(media.picture.at, now) && media.picture.revision === target?.geometry.revision;
  const allowed = live && online && caughtUp && grants?.control_input && grants.tasks && !reason && !view?.private_step && fresh;
  // Drawing on the picture goes with the hand-back, so it needs the same
  // permissions, and a capture the agent's own run can be sent.
  const markable = live && online && caughtUp && grants?.control_input && grants.tasks && !view?.private_step && target?.capture.state === "available";
  if (!live || !target || (!takenOver && !review)) return null;
  /** Capture what the target shows now into the task's markup and open it.
   * Its Review sends the drawing and gives the input back together. */
  const markUp = async () => {
    if (!markable || !run) return;
    setCapturing(true);
    setError(null);
    try {
      const composer = ui.draft(task.project_id, task.task_id);
      if (composer?.attempt) throw new PendingMessageError(draftKey(task.project_id, task.task_id));
      const store = annotationStore(profile.profileId);
      const existing = composer?.annotationId ? store.get()[composer.annotationId] : undefined;
      const id = existing?.projectId === task.project_id && existing.taskId === task.task_id ? existing.id : store.create(task.project_id, task.task_id).id;
      await captureSource({ platform, profileId: profile.profileId, session }, store, id, target.target_id);
      ui.saveDraft(task.project_id, task.task_id, { annotationId: id });
      const chosen = defaultIntent(composerMode(task, run, harness ?? null));
      close();
      ui.push({ screen: "annotate", annotationId: id, intent: chosen.kind === "message" ? chosen.delivery : "next_turn", handBack: { targetId: target.target_id, runId: run.run_id } });
    } catch (e) { setError(describeError(e, profile.name)); }
    finally { setCapturing(false); }
  };
  return <>
    {takenOver && !reviewOnly && <div className="talk-handback">
      <p>{mine ? `You’re in control of ${target.label} · the agent is paused`
        : `${releasedTakeover(view) ? "Your control ended" : "Agent paused"} · it can’t use ${target.label} until you resume it`}</p>
      <Button size="sm" onClick={() => setReview(true)}>Resume agent</Button>
    </div>}
    <Sheet open={review} onClose={close} title={`Resume the agent on ${target.label}`} footer={<>
      <Button icon={Pencil} disabled={!markable || busy} pending={capturing} onClick={() => void markUp()}>Mark up</Button>
      <Button variant="primary" disabled={!allowed || capturing} pending={busy} onClick={async () => {
        if (!allowed || !run) return;
        setBusy(true);
        setError(null);
        try {
          if (await returnToAgent(session, execute, { taskId: task.task_id, runId: run.run_id, targetId: target.target_id, view, mine })) {
            close();
            if (!watching) ui.push({ screen: "computer_use", taskId: task.task_id }, "control");
          }
        } catch (e) { setError(describeError(e, profile.name)); }
        finally { setBusy(false); }
      }}>Resume now</Button>
    </>}>
      <p>Review the current picture and remove anything sensitive. The agent will receive a fresh observation. To show it something, Mark up draws on a capture and sends it as the agent resumes.{!watching && " It acts only while you watch its screen, so that opens next."}</p>
      {error && <p role="alert">{error}</p>}
      {reason ? <><p role="status">{reason}</p>{view?.desktop_lock === "locked" && <Unlock />}</> : <LivePicture target={target} canvas={media.canvas} backdrop={media.backdrop} onRegion={media.setRegion} picture={media.picture} retry={media.retry} compact />}
    </Sheet>
  </>;
}

/** Where a waiting target is shown, the same review hands its input back.
 * It is what a paused agent is waiting for, so it leads, Take over beside it. */
export function GiveInputBack({ task }: { task: Task }) {
  const [open, setOpen] = useState(false);
  return <>
    <Button className="intervention" variant="primary" icon={Bot} onClick={() => setOpen(true)}>Resume agent</Button>
    {open && <HandBack task={task} reviewOnly watching onClose={() => setOpen(false)} />}
  </>;
}
