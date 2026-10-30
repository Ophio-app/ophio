// SPDX-License-Identifier: Apache-2.0
import type { ControlTarget, Run, TargetControlView, Task } from "../protocol/types";

/** Input paused because a person's control ended, not because of Pause input. */
export function releasedTakeover(view: TargetControlView | undefined): boolean {
  return view?.input_state === "paused" && view.lease?.owner.kind === "human";
}

/** Paused input belongs to this run: the agent's own lease was paused, or a
 * person's control ended while this run was the agent watching the target. */
function waitsForInput(view: TargetControlView | undefined, runId: string): boolean {
  if (view?.input_state !== "paused" || view.private_step) return false;
  const owner = view.lease?.owner;
  return owner?.kind === "agent" ? owner.run_id === runId : view.model_observation?.run_id === runId;
}

/** This task's live run is waiting to have input on its target given back.
 * Only the control view and the run decide it, so a message to the task
 * leaves it in place; giving input back or the run ending clears it. */
export function awaitingInputBack(task: Task, run: Run | undefined, view: TargetControlView | undefined): boolean {
  return !!run && run.run_id === task.current_run && run.task_id === task.task_id && run.ended_at === null
    && ["working", "awaiting_input", "awaiting_approval"].includes(run.state) && waitsForInput(view, run.run_id);
}

export function handBackTarget(state: {
  runs: Record<string, Run>; control: Record<string, TargetControlView>; targets: ControlTarget[];
}, task: Task): ControlTarget | undefined {
  return awaitingInputBack(task, state.runs[task.current_run ?? ""], state.control[task.target_id ?? ""])
    ? state.targets.find(t => t.target_id === task.target_id) : undefined;
}

export const handBackReason = (target: ControlTarget) => `Agent paused on ${target.label} · Resume agent`;
