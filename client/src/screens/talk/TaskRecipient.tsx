// SPDX-License-Identifier: Apache-2.0
import { launchSummary } from "../../app/launchPolicy";
import { useState } from "react";
import { useHost, useUi } from "../../app/context";
import { draftKey, type AgentChoice } from "../../app/ui";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import type { Project, Task } from "../../protocol/types";
import { agentLabel } from "./intent";

function useRecipient(current: AgentChoice | null, wanted: AgentChoice | null, identity: string | undefined) {
  const harnesses = useHost(s => s.harnesses);
  const mismatch = !!current && !!wanted && (current.harness !== wanted.harness || current.model !== wanted.model || current.effort !== wanted.effort || (wanted.launchPolicy !== undefined && (current.launchPolicy ?? null) !== wanted.launchPolicy));
  const key = JSON.stringify([identity, current, wanted]);
  const [accepted, setAccepted] = useState<string | null>(null);
  return {
    current: current ? agentLabel(harnesses, current) : null,
    wanted: wanted ? agentLabel(harnesses, wanted) : null,
    wantedLaunch: wanted?.launchPolicy ? launchSummary(wanted.harness, wanted.launchPolicy, "work") : null,
    policyOnly: !!current && !!wanted && current.harness === wanted.harness && current.model === wanted.model && current.effort === wanted.effort,
    blocked: mismatch && accepted !== key,
    keep: () => setAccepted(key),
  };
}

/** A new-task selection must not look like a change to an existing session.
 * A conversation's chip changes its model for the next message, so it asks nothing. */
export function useTaskRecipient(project: Project | undefined, task: Task | null | undefined) {
  const chosen = useUi(s => project ? s.drafts[draftKey(project.project_id, task?.task_id ?? null)]?.agent
    ?? s.drafts[draftKey(project.project_id, null)]?.agent : undefined);
  const current = task ? { harness: task.harness, model: task.model, effort: task.effort, launchPolicy: task.launch_policy ?? null } : null;
  return useRecipient(current, task?.conversation ? null : chosen ?? null, task?.task_id);
}

/** Replaying a lost request uses its original recipient, not an edited draft's. */
export function useRetryRecipient(projectId: string, taskId: string | null) {
  const draft = useUi(s => s.drafts[draftKey(projectId, taskId)]);
  const next = useUi(s => s.drafts[draftKey(projectId, null)]?.agent);
  const attempt = draft?.attempt;
  const body = attempt?.phase === "unknown" ? attempt.body : null;
  const task = useHost(s => body?.command === "resume_task" || body?.command === "send_message" ? s.tasks[body.task_id] : undefined);
  const current = body?.command === "create_task" ? { harness: body.harness, model: body.model, effort: body.effort, launchPolicy: body.launch_policy ?? null }
    : task ? { harness: task.harness, model: task.model, effort: task.effort, launchPolicy: task.launch_policy ?? null } : null;
  // A lost conversation message carries its own model and effort.
  return useRecipient(current, task?.conversation ? null : draft?.agent ?? (task ? next : null) ?? null, attempt?.commandId ?? undefined);
}

/** Says nothing while the message goes where the agent chip says. Only a
 * selection that differs from the task's own agent asks which one to use. */
export function TaskRecipient({ recipient, onNewTask, retry = false }: { recipient: ReturnType<typeof useTaskRecipient>; onNewTask?: () => void; retry?: boolean }) {
  if (!recipient.current || !recipient.blocked) return null;
  return <div className="composer__recipient">
    <Notice tone="warn" title={recipient.policyOnly ? (retry ? "The earlier request uses a different permission setting" : "This task uses a different permission setting") : (retry ? "The earlier request uses a different agent or model" : "This task uses a different agent or model")}>
      <p>Your selection: {recipient.wanted}. {retry ? "Send again keeps the original request’s agent." : "Continue keeps this task’s agent."}</p>
      <p>{retry ? "Check whether it arrived, or confirm its original agent before repeating it." : "A new task sends only the new request and attachments, not this conversation."}</p>
      {recipient.wantedLaunch && <p style={{ overflowWrap: "anywhere" }}>Chosen for Work: {recipient.wantedLaunch}. Continuing or repeating keeps the saved setting.</p>}
      <div className="work-actions">
        {onNewTask && <Button variant="secondary" onClick={onNewTask}>New task with this selection</Button>}
        <Button variant="quiet" onClick={recipient.keep}>{retry ? "Keep the original request’s agent" : "Keep this task’s agent"}</Button>
      </div>
    </Notice>
  </div>;
}
