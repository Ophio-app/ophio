// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { useCommand, useHost } from "../../app/context";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import type { Reconciliation, Run, Task } from "../../protocol/types";
import { CommandFeedback, useCanWork, useUnconfirmedCommand } from "./shared";

/** Recording what happened to a run whose outcome the computer couldn't confirm. */
export function useReconcile(task: Task, onDone?: () => void) {
  const send = useCommand();
  const can = useCanWork();
  const uncertain = useUnconfirmedCommand((c) => c.command === "reconcile_task" && c.task_id === task.task_id);
  const current = useHost((s) => s.tasks[task.task_id]);
  const [pending, setPending] = useState(false);
  const reconcile = async (resolution: Reconciliation) => {
    setPending(true);
    const r = await send(
      { command: "reconcile_task", task_id: task.task_id, resolution },
      { toastErrors: false, label: `Reconcile ${task.title}`, expectedRevision: task.revision },
    );
    setPending(false);
    if (r) {
      announce(resolution === "mark_completed" ? `Recorded “${task.title}” as completed.` : `Recorded “${task.title}” as failed.`);
      onDone?.();
    }
  };
  return { reconcile, pending, disabled: !can || uncertain || current?.revision !== task.revision };
}

/** How long the note that a decision was recorded holds the place of what
 * the run came to, so a second tap on Mark failed or Mark completed can't land
 * on Archive or Try again. */
const RECORDED_MS = 2000;

/** The decision recorded on `run` while it was on screen, until its note has shown. */
export function useJustRecorded(run: Run | undefined): Run["human_resolution"] {
  const decided = !!run?.human_resolution;
  const [seen, setSeen] = useState({ run: run?.run_id, decided });
  const [recorded, setRecorded] = useState(false);
  if (seen.run !== run?.run_id || seen.decided !== decided) {
    setSeen({ run: run?.run_id, decided });
    setRecorded(seen.run === run?.run_id && decided);
  }
  useEffect(() => {
    if (!recorded) return;
    const timer = setTimeout(() => setRecorded(false), RECORDED_MS);
    return () => clearTimeout(timer);
  }, [recorded]);
  return recorded ? run?.human_resolution : undefined;
}

/** Where on the receipt to look before deciding, and how much is there. */
export interface ReceiptParts { changes: number; steps: boolean; helpers: number }

/**
 * Why a run waits for the person's decision, and what the receipt below can
 * show them first: what the harness last said, the files it changed, its
 * steps and its helpers. The computer can't ask the agent while the outcome
 * is unknown; once it is recorded, Summarize can.
 */
export function ReconcileNote({ task, parts, onSee }: { task: Task; parts: ReceiptParts; onSee: (part: keyof ReceiptParts) => void }) {
  return (
    <>
      <Notice tone="unknown" title="The outcome needs your decision">
        <p>
          This run ended without a known result. Look over what it did below, then record what you find{(task.mode ?? "work") === "work" ? "; until you do, nothing else can change files in this project." : "."}
        </p>
        {task.status_line && <p>Last progress: {task.status_line}</p>}
        <div className="work-actions">
          <Button size="sm" onClick={() => onSee("changes")}>{parts.changes ? `Changed files · ${parts.changes}` : "Changed files"}</Button>
          {parts.steps && <Button size="sm" onClick={() => onSee("steps")}>Steps</Button>}
          {parts.helpers > 0 && <Button size="sm" onClick={() => onSee("helpers")}>{`Helpers · ${parts.helpers}`}</Button>}
        </div>
      </Notice>
      <p className="work-muted">
        Mark completed or Mark failed records your decision for this run and its helper unknowns without verifying the work; afterward, continue from Chat and say what to redo.
      </p>
      <CommandFeedback match={(c) => c.command === "reconcile_task" && c.task_id === task.task_id} />
    </>
  );
}

export function ReconcileButtons({ task, onDone }: { task: Task; onDone?: () => void }) {
  const { reconcile, pending, disabled } = useReconcile(task, onDone);
  return (
    <>
      <Button variant="secondary" disabled={disabled} pending={pending} onClick={() => void reconcile("mark_failed")}>
        Mark failed
      </Button>
      <Button variant="secondary" disabled={disabled} pending={pending} onClick={() => void reconcile("mark_completed")}>
        Mark completed
      </Button>
    </>
  );
}
