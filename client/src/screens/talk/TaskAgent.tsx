// SPDX-License-Identifier: Apache-2.0
//
// The agent, model and effort a task's messages go to, as a chip beside how
// they are sent. Its sheet holds what Talk itself leaves out: what the agent
// reported running, the task's saved launch flags and branch, and how to start
// work with a different agent. A task keeps its agent, so nothing here changes it.

import { ChevronDown, Plus } from "lucide-react";
import { useHost } from "../../app/context";
import { launchRisk, launchSummary } from "../../app/launchPolicy";
import { startedLabel } from "../../app/launchers";
import { Chip } from "../../design/Chip";
import { ListRow, Section } from "../../design/ListRow";
import { Sheet } from "../../design/Sheet";
import type { Run, Task } from "../../protocol/types";
import { agentLabel, runAgent } from "./intent";
import { LaunchLine } from "./LaunchLine";

/** What the task's chip and sheet say: the agent as reported, else as asked. */
export function useTaskAgent(task: Task | null, run: Run | null) {
  const harnesses = useHost((s) => s.harnesses);
  if (!task) return null;
  const reported = run ? runAgent(harnesses, run) : null;
  return {
    label: reported?.label ?? agentLabel(harnesses, { harness: task.harness, model: task.model, effort: task.effort }),
    status: reported?.differs ?? (reported?.requestedOnly ? "Requested; the agent hasn’t said what it runs yet" : "Messages in this task go to this agent"),
    flags: launchSummary(task.harness, task.launch_policy, task.mode ?? "work"),
    risk: launchRisk(task.harness, task.launch_policy, task.mode ?? "work"),
    launch: { harness: task.harness, policy: task.launch_policy, mode: task.mode ?? "work" },
    branch: task.branch ?? null,
    started: startedLabel(task.start),
  };
}

export function TaskAgentChip({ agent, onClick }: { agent: NonNullable<ReturnType<typeof useTaskAgent>>; onClick: () => void }) {
  return <Chip className="composer__chip composer__agent composer__opt composer__extra" onClick={onClick} aria-haspopup="dialog">
    <span className="visually-hidden">Agent: </span>{agent.label}
    {agent.started && <span className="composer__started"> · {agent.started}</span>}
    <ChevronDown className="composer__chip-chev" aria-hidden />
  </Chip>;
}

export function TaskAgentSheet({ open, onClose, agent, onNewTask }: { open: boolean; onClose: () => void; agent: NonNullable<ReturnType<typeof useTaskAgent>>; onNewTask?: () => void }) {
  return <Sheet open={open} onClose={onClose} title="This task’s agent">
    <Section>
      <div className="group">
        <ListRow title={agent.label} subtitle={agent.status} />
        {agent.started && <ListRow title="Started from" subtitle={agent.started} className="talk-agent__flags" />}
        <ListRow title="Permission prompts" subtitle={agent.risk ? `${agent.flags}. ${agent.risk}` : agent.flags} className="talk-agent__flags" />
        {agent.branch && <ListRow title="Branch" subtitle={agent.branch.new ? `The agent is told to make ${agent.branch.name} and work there.` : `The agent is told to work on ${agent.branch.name}.`} className="talk-agent__flags" />}
      </div>
      <LaunchLine className="fine talk-agent__note" {...agent.launch} label={false} risk={false} />
      <p className="fine talk-agent__note">A task keeps its agent, permission setting and branch{agent.started ? ", and what it started from" : ""}. To use different ones, start a new task; what you’ve typed comes with you.</p>
    </Section>
    {onNewTask && <Section>
      <div className="group">
        <ListRow icon={Plus} title="New task" subtitle="Choose its agent, model, effort and flags" chevron onClick={() => { onClose(); onNewTask(); }} />
      </div>
    </Section>}
  </Sheet>;
}
