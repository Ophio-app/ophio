// SPDX-License-Identifier: Apache-2.0
// An ordinary agent looking into a problem report from the owner's inbox,
// where no contained fixer runs. A report can come from anyone, so the gate
// is Plan mode: the agent reads and proposes, and changes nothing until the
// person reads its plan and chooses Carry out this plan. The computer, not
// this screen, writes the agent's prompt from the report.
import { Bug, Folder } from "lucide-react";
import { useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { ListRow } from "../../design/ListRow";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import { REPORT_LOOK_INTO_FEATURE, REPORT_TASK_FEATURE } from "../../diagnostics/constants";
import { commandResult } from "../../host/commandResult";
import type { Project, Task } from "../../protocol/types";

/** The task still open for a report: the newest not put away. */
function openTaskFor(tasks: Record<string, Task>, reportId: string): Task | undefined {
  return Object.values(tasks)
    .filter((t) => t.report?.report_id === reportId && t.state !== "archived")
    .sort((a, b) => b.created_at - a.created_at)[0];
}

/** On a report's screen: the one button, or the agent already on it. */
export function ReportTask({ reportId, open }: { reportId: string; open: boolean }) {
  const { session, ui, platform, profile } = useApp();
  const online = useLink().state === "online";
  const supported = useHost((s) => {
    const features = s.capabilities?.features ?? [];
    return features.includes(REPORT_TASK_FEATURE) && !features.includes(REPORT_LOOK_INTO_FEATURE);
  });
  const allowed = useHost((s) => s.grants?.admin === true && s.grants.tasks === true);
  const task = useHost((s) => openTaskFor(s.tasks, reportId));
  const projects = useHost((s) => s.projects);
  const [busy, setBusy] = useState(false);
  const [choosing, setChoosing] = useState(false);

  if (task) {
    return <section className="work-stack" aria-label="Agent looking into this report">
      <p className="work-card-title">An agent is looking into this</p>
      <p className="work-caption">It reads the report and Ophio’s source, then proposes a fix. Nothing changes until you read its plan and choose Carry out this plan.</p>
      <div className="work-actions">
        <Button onClick={() => ui.openTask(task.project_id, task.task_id)}>Open “{task.title}”</Button>
      </div>
    </section>;
  }
  if (!supported || !open) return null;

  const start = async (projectId: string | null) => {
    setBusy(true);
    setChoosing(false);
    platform.haptic?.("tap");
    try {
      // The computer finds the project that holds Ophio's source, or uses
      // the one chosen last time. When it can't tell, the person chooses.
      const found = projectId ?? (await session.query({ kind: "report_task_project" })).project_id;
      if (!found) { setChoosing(true); return; }
      // Only the person's own choice is sent, and the computer keeps it.
      const receipt = await session.command(
        { command: "start_report_task", report_id: reportId, ...(projectId ? { project_id: projectId } : {}) },
        { label: "Have an agent look into a report" },
      );
      const made = await commandResult(session, receipt);
      if (made?.kind !== "task_created") throw new Error("It may have started. Check Tasks before asking again.");
      // A task already open for the report may be in another project.
      const madeIn = session.state.get().tasks[made.task_id]?.project_id ?? found;
      ui.openTask(madeIn, made.task_id);
      ui.selectCreated(madeIn, made.task_id);
    } catch (e) {
      toast(describeError(e, profile.name), { tone: "stop" });
    } finally {
      setBusy(false);
    }
  };

  const choices = Object.values(projects).filter((p: Project) => !p.one_off).sort((a, b) => a.name.localeCompare(b.name));
  return <section className="work-stack" aria-label="Have an agent look into this report">
    <p className="work-caption">An agent reads the report and Ophio’s source, then proposes a fix. It can’t change anything until you read its plan and choose Carry out this plan.</p>
    <div className="work-actions">
      <Button variant="primary" pending={busy} disabled={!online || !allowed || busy} onClick={() => void start(null)}>Have an agent look into it</Button>
      {/* The computer remembers the project; this is how to change it. */}
      <Button variant="quiet" disabled={!online || !allowed || busy} onClick={() => setChoosing(true)}>Use another project</Button>
    </div>
    {!allowed && <p className="work-muted">This phone doesn’t have permission to start tasks or answer reports.</p>}
    <Sheet open={choosing} title="Which project holds Ophio’s source?" subtitle="The agent works there. Ophio remembers your choice." onClose={() => setChoosing(false)}>
      {choices.length === 0
        ? <p className="work-muted">Add the project that holds Ophio’s source on the computer first.</p>
        : choices.map((p) => <ListRow key={p.project_id} icon={Folder} title={p.name} subtitle={p.host_path} chevron onClick={() => void start(p.project_id)} />)}
    </Sheet>
  </section>;
}

/** The report a task looks into, and a way back to it while it's in the inbox. */
function useReportItem(task: Task) {
  return useHost((s) => Object.values(s.attention).find((a) =>
    a.report_id === task.report?.report_id && a.task_id === null && a.kind === "waiting_for_you" && !a.resolved_at));
}

/** On a task's receipt: which report it is about. */
export function ReportLine({ task }: { task: Task }) {
  const { ui } = useApp();
  const item = useReportItem(task);
  if (!task.report) return null;
  return <div className="work-actions">
    <p className="work-caption">Looking into problem report {task.report.reference}</p>
    {item && <Button variant="quiet" size="sm" onClick={() => ui.push({ screen: "attention", attentionId: item.attention_id }, "work")}>Open report</Button>}
  </div>;
}

/** Under a task's chat header: which report it is about. */
export function ReportPin({ task }: { task: Task }) {
  const { ui } = useApp();
  const item = useReportItem(task);
  if (!task.report) return null;
  return <div className="chat-watch" role="group" aria-label="Problem report">
    <Bug className="chat-watch__icon" aria-hidden />
    <span className="chat-watch__text">Looking into problem report {task.report.reference}</span>
    {item && <Button variant="quiet" size="sm" className="chat-watch__stop" onClick={() => ui.push({ screen: "attention", attentionId: item.attention_id }, "work")}>Open report</Button>}
  </div>;
}
