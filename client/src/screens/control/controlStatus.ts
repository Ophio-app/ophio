// SPDX-License-Identifier: Apache-2.0
import type { Run, TargetControlView, Task } from "../../protocol/types";
import { awaitingInputBack, releasedTakeover } from "../../state/handBack";
export { releasedTakeover } from "../../state/handBack";
export function holdsHumanInput(view: TargetControlView | undefined, deviceId: string | null): boolean {
  return view?.input_state === "human" && view.lease?.state === "active" && view.lease.owner.kind === "human" && view.lease.owner.device_id === deviceId;
}
export function controlStatus(task: Task, run: Run | undefined, view: TargetControlView | undefined): string | null {
  if (!awaitingInputBack(task, run, view)) return task.status_line;
  return `${releasedTakeover(view) ? "Your control ended" : "Agent paused"} · resume it when you’re ready`;
}
