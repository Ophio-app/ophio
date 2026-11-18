// SPDX-License-Identifier: Apache-2.0
import { useRef, useState } from "react";
import { useCheckActions, useChecks } from "../../app/checks";
import { CHECKS_FEATURE } from "../../app/checksFeature";
import { useApp, useHost } from "../../app/context";
import { Button } from "../../design/Button";
import { toast } from "../../design/Toast";
import type { Task } from "../../protocol/types";
import type { HostState } from "../../state/projection";
import { hasEndedTask, latestRun } from "./logic";
import { useLocalText } from "./local";

/** Keep the offer on one finished Work card, not every old task in a project.
 * Use the run's finish time so renaming an old task does not move the offer. */
export function latestSetupTask(state: Pick<HostState, "tasks" | "runs">, projectId: string): string | null {
  const finished = Object.values(state.tasks).filter(t => t.project_id === projectId && t.kind === "coding" && !t.conversation && !t.review && !t.asked_by && (t.mode ?? "work") === "work" && !t.checking && hasEndedTask(t));
  finished.sort((a, b) => (latestRun(b, state.runs)?.ended_at ?? b.updated_at) - (latestRun(a, state.runs)?.ended_at ?? a.updated_at) || b.task_id.localeCompare(a.task_id));
  return finished[0]?.task_id ?? null;
}

export function SetupChecksOffer({ task }: { task: Task }) {
  const candidate = useHost(s => s.capabilities?.features.includes(CHECKS_FEATURE) === true && s.grants?.tasks === true && latestSetupTask(s, task.project_id) === task.task_id);
  return candidate ? <ProjectSetupOffer key={task.project_id} projectId={task.project_id} /> : null;
}

function ProjectSetupOffer({ projectId }: { projectId: string }) {
  const { profile } = useApp();
  const [dismissed, remember] = useLocalText(`${profile.profileId}.checks-offer.${projectId}`);
  const dismiss = () => {
    if (!remember("dismissed")) toast("This phone could not save your choice. The offer may appear again.", { tone: "stop" });
  };
  return dismissed ? null : <AvailableSetupOffer projectId={projectId} dismiss={dismiss} />;
}

function AvailableSetupOffer({ projectId, dismiss }: { projectId: string; dismiss: () => void }) {
  const checks = useChecks(projectId);
  const actions = useCheckActions();
  const writable = useHost(s => s.grants?.library_write === true);
  const [pending, setPending] = useState(false);
  const sending = useRef(false);
  const project = checks.projects.find(p => p.project_id === projectId);
  const suggestions = project?.drafts.some(d => ["proving", "ready", "stale"].includes(d.draft.state));
  if (!checks.supported || !checks.allowed || !checks.online || checks.loading || checks.error || !project || project.approved.checks.length > 0 || suggestions) return null;
  const setup = async () => {
    if (!writable || sending.current || !checks.online) return;
    sending.current = true;
    setPending(true);
    try {
      if (await actions.ask(projectId, null, "")) dismiss();
    } finally {
      sending.current = false;
      setPending(false);
    }
  };
  return <div className="checks-setup-offer work-stack">
    <p>Want Ophio to check the work itself? Set up checks so you do not have to take the agent’s word for it.</p>
    <div className="work-actions">
      <Button size="sm" pending={pending} disabled={!writable || pending} onClick={() => void setup()}>Set up checks</Button>
      <Button size="sm" disabled={pending} onClick={dismiss}>Dismiss</Button>
    </div>
    {!writable && <p className="work-caption">Library editing permission is needed to set up checks.</p>}
  </div>;
}
