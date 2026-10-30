// SPDX-License-Identifier: Apache-2.0
import type { ControlTarget, TargetControlView, Task } from "../../protocol/types";

/** Resolve identity independently of which controls a screen already offers. */
export function taskDisplayId(task: Task, views: TargetControlView[]): string | null {
  const held = task.current_run && views.find(v => v.lease?.owner.kind === "agent" && v.lease.owner.run_id === task.current_run);
  return (held ? held.target_id : null) ?? task.target_id
    ?? (task.current_run ? views.find(v => v.model_observation?.run_id === task.current_run)?.target_id : null) ?? null;
}

export function taskDisplayLabel(task: Task, views: TargetControlView[], targets: ControlTarget[], online: boolean, focusedTarget?: string | null): string {
  const id = taskDisplayId(task, views);
  // Beside a caption that names a display, a Stop says outright when its task
  // uses none, so no one reads it as stopping whatever holds that display.
  if (!online) return "display unknown until reconnected";
  if (!id) return "not using a display";
  const name = id === focusedTarget ? "this display" : targets.find(t => t.target_id === id)?.label ?? "Display unavailable";
  const view = views.find(v => v.target_id === id);
  const state = !view || view.input_state === "unknown" ? "input unconfirmed"
    : view.desktop_lock === "locked" ? "computer locked"
    : view.input_state === "paused" ? "agent paused"
    : view.input_state === "ready" ? "no one has input" : null;
  return `${name}${state ? ` · ${state}` : ""}`;
}

/** A reported agent lease is stronger evidence than a task's target hint. */
export function holdsDisplay(task: Task, view: TargetControlView): boolean {
  if (view.lease?.owner.kind === "agent") return view.lease.owner.run_id === task.current_run;
  return task.target_id === view.target_id || (!!task.current_run && view.model_observation?.run_id === task.current_run);
}
