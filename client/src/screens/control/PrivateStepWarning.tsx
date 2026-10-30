// SPDX-License-Identifier: Apache-2.0
import { useApp, useHost } from "../../app/context";
import type { Route } from "../../app/ui";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import type { HostState } from "../../state/projection";

export function privateStepTask(state: HostState, targetId: string) {
  const view = state.control[targetId];
  if (!view?.private_step) return undefined;
  const runId = view.model_observation?.run_id ?? (view.lease?.owner.kind === "agent" ? view.lease.owner.run_id : undefined);
  const receiptTask = state.interventions.filter(r => r.targetId === targetId && r.taskId).at(-1)?.taskId;
  return state.tasks[(runId ? state.runs[runId]?.task_id : receiptTask) ?? ""];
}

export function privateStepRoute(state: HostState | undefined, targetId: string): Route {
  const task = state && privateStepTask(state, targetId);
  return task ? { screen: "computer_use", taskId: task.task_id } : { screen: "desktop", targetId };
}

export function PrivateStepWarning({ targetId }: { targetId: string }) {
  const { ui } = useApp();
  const step = useHost(s => s.control[targetId]?.private_step);
  const task = useHost(s => privateStepTask(s, targetId));
  const target = useHost(s => s.targets.find(t => t.target_id === targetId));
  if (!step) return null;
  return <Notice tone={step.ready ? "info" : "unknown"} title={`${target?.label ?? "Display"} · Private step needs review`}
    action={task && <Button onClick={() => ui.push({ screen: "computer_use", taskId: task.task_id })}>Review private step</Button>}>
    {step.ready ? "The private step is still open." : "Privacy is not confirmed. Review what the computer could stop before entering anything private."}
  </Notice>;
}
