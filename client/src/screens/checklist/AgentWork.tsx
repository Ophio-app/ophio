// SPDX-License-Identifier: Apache-2.0
// Task-side entries in To-Do. The computer remains the owner of these tasks:
// the person can open a request or send words to the task, but cannot tick it.
import type { ReactNode } from "react";
import { useApp, useHost, shallowEqual } from "../../app/context";
import { Button } from "../../design/Button";
import { Pill } from "../../design/Chip";
import { ListRow, Section } from "../../design/ListRow";
import type { Checklist, ChecklistItem, QueueView, Task } from "../../protocol/types";
import type { HostState } from "../../state/projection";
import { listedAttention } from "../work/shared";
import { pendingQueueItems } from "../work/useQueue";

export interface AgentWorkEntryData {
  id: string;
  projectId: string;
  tier: 0 | 2;
  title: string;
  taskId: string;
  kind: "attention" | "waiting" | "next";
  detail: string;
  attentionId?: string;
  checklistItemId?: string;
  /** An accepted task has no running turn to deliver text to. */
  feedback: boolean;
}

const ongoing = (task: Task) => !["completed", "failed", "interrupted", "archived"].includes(task.state);
const personItem = (item: ChecklistItem) => item.owner !== "agent";

/** Derive from the latest host projection and the currently displayed checklist.
 * The latter may be an optimistic pending tick: once its item is done the
 * corresponding waiting row disappears without waiting for the host to reply. */
export function deriveAgentWork(state: HostState, projectId: string, list: Checklist | null | undefined, queue: QueueView | undefined = state.queues[projectId]): AgentWorkEntryData[] {
  const tasks = state.tasks;
  const result: AgentWorkEntryData[] = [];
  const covered = new Set<string>();
  const add = (entry: AgentWorkEntryData) => { result.push(entry); covered.add(entry.taskId); };

  // Use precisely the open decisions shown in Work's Needs you list. Opening
  // the alert itself preserves approvals, questions, and queue go-aheads.
  for (const attention of listedAttention(state, projectId)) {
    const task = tasks[attention.task_id!];
    if (!task || covered.has(task.task_id)) continue;
    add({ id: `attention:${attention.attention_id}`, projectId, tier: 0,
      title: task.title, taskId: task.task_id, kind: "attention",
      detail: attention.body ?? (attention.kind === "approval" ? "Approval needed" : attention.kind === "question" ? "Question waiting" : "Needs you"), attentionId: attention.attention_id,
      feedback: task.state !== "accepted" && task.state !== "dispatching" });
  }

  const items = list?.sections.flatMap(section => section.items) ?? [];
  const byId = new Map(items.map(item => [item.item_id, item]));
  for (const item of items) {
    if (item.done_at !== undefined || !item.task_id) continue;
    const task = tasks[item.task_id];
    if (!task || task.project_id !== projectId || !ongoing(task) || covered.has(task.task_id)) continue;
    // A person's open handoff blocks this writer. Agent-owned work with
    // prerequisites links to the first unfinished person's item instead.
    const prerequisite = item.owner === "agent"
      ? item.after?.map(id => byId.get(id)).find((before): before is ChecklistItem => !!before && personItem(before) && before.done_at === undefined)
      : undefined;
    if (personItem(item) && !item.waiting || item.owner === "agent" && !prerequisite) continue;
    const blocker = prerequisite ?? item;
    add({ id: `waiting:${task.task_id}:${blocker.item_id}`, projectId, tier: 0,
      title: task.title, taskId: task.task_id, kind: "waiting",
      detail: `Waiting on you to finish “${blocker.title}”`, checklistItemId: blocker.item_id,
      feedback: task.state !== "accepted" && task.state !== "dispatching" });
  }

  for (const queued of pendingQueueItems(queue)) {
    const task = tasks[queued.task_id];
    if (!task || task.project_id !== projectId || !ongoing(task) || covered.has(task.task_id)) continue;
    add({ id: `next:${queued.item_id}`, projectId, tier: 2,
      title: task.title, taskId: task.task_id, kind: "next", detail: "Not started",
      feedback: false });
  }
  return result;
}

export function useAgentWork(projectId: string, list: Checklist | null | undefined): AgentWorkEntryData[] {
  return useHost(state => deriveAgentWork(state, projectId, list), shallowEqual);
}

export function AgentWorkEntry({ entry, onChecklist, projectLabel }: {
  entry: AgentWorkEntryData;
  onChecklist: (projectId: string, itemId?: string) => void;
  projectLabel?: ReactNode;
}) {
  const { ui } = useApp();
  const open = () => {
    if (entry.kind === "attention" && entry.attentionId) ui.push({ screen: "attention", attentionId: entry.attentionId }, "work");
    else if (entry.checklistItemId) onChecklist(entry.projectId, entry.checklistItemId);
    else { ui.contextProject(entry.projectId); ui.push({ screen: "next_up" }, "work"); }
  };
  return <li className="agent-work-entry">
    {projectLabel && <div className="agent-work-entry__project">{projectLabel}</div>}
    <ListRow title={entry.title} subtitle={<><Pill tone={entry.kind === "attention" ? "neutral" : "accent"}>{entry.kind === "attention" ? "For you" : "For agent"}</Pill> {entry.detail}</>} chevron onClick={open} />
    <div className="agent-work-entry__actions">
      {entry.feedback
        ? <Button size="sm" variant="quiet" onClick={() => ui.openTask(entry.projectId, entry.taskId)}>Send feedback</Button>
        : entry.kind === "next" && <Button size="sm" variant="quiet" onClick={open}>Open in Next up</Button>}
    </div>
  </li>;
}

export function AgentWork({ projectId, list, onChecklist }: {
  projectId: string;
  list: Checklist | null | undefined;
  onChecklist: (projectId: string, itemId?: string) => void;
}) {
  const entries = useAgentWork(projectId, list);
  if (!entries.length) return null;
  return <Section title="Agents’ work"><ul className="agent-work-list">
    {entries.map(entry => <AgentWorkEntry key={entry.id} entry={entry} onChecklist={onChecklist} />)}
  </ul></Section>;
}
