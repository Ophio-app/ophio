// SPDX-License-Identifier: Apache-2.0
import type { Project, Task } from "../protocol/types";
import { projectTasks, runningTasks, type HostState } from "../state/projection";
import type { UiState, UiStore } from "./ui";
import { useApp, useHost, useUi } from "./context";
import { useLayoutEffect } from "react";

export function talkProject(state: HostState, chosen: string | null): Project | null {
  const recent = Object.values(state.tasks).sort((a, b) => b.updated_at - a.updated_at)[0];
  return (chosen ? state.projects[chosen] : undefined)
    ?? (recent ? state.projects[recent.project_id] : undefined)
    ?? Object.values(state.projects)[0] ?? null;
}

/** Undefined means no choice yet, and Talk picks up a conversation still
 * going; null is an intentional new-task or new-conversation draft. */
export function talkTask(state: HostState, projectId: string, selected: string | null | undefined): Task | null {
  const tasks = projectTasks(state, projectId);
  return selected === undefined
    ? tasks.find((t) => t.conversation && ["working", "awaiting_decision", "dispatching"].includes(t.state)) ?? null
    : tasks.find((t) => t.task_id === selected) ?? null;
}

/** What Talk's root shows: its conversations, one of them, or a task or new
 * task opened there, which Back leaves for Work, where tasks are. */
export type TalkShows = "list" | "conversation" | "task";
export function talkShows(state: HostState, ui: Pick<UiState, "projectId" | "taskByProject" | "taskList" | "conversing">): TalkShows {
  const project = talkProject(state, ui.projectId);
  if (ui.taskList || !project) return "list";
  const selected = ui.taskByProject[project.project_id];
  if (selected === undefined) return talkTask(state, project.project_id, undefined) ? "conversation" : "list";
  const task = selected === null ? undefined : state.tasks[selected];
  return (task ? task.conversation : ui.conversing) ? "conversation" : "task";
}

/** The initial suggestion becomes the conversation as soon as Talk shows it. */
export function usePinnedTalkTask(projectId: string): Task | null {
  const { ui } = useApp();
  const selected = useUi(s => s.taskByProject[projectId]);
  const visible = useUi(s => s.tab === "talk" && s.stacks.talk.at(-1)?.screen === "talk" && !s.taskList);
  const task = useHost(s => talkTask(s, projectId, selected));
  useLayoutEffect(() => {
    if (visible && selected === undefined && task) ui.selectTask(projectId, task.task_id);
  }, [visible, selected, task, projectId, ui]);
  return task;
}

/** Stop can also originate in the rail above Talk, before a host update. */
export function pinStoppedConversation(state: HostState, ui: UiStore, task: Task): void {
  const local = ui.get();
  if (local.stacks[local.tab].at(-1)?.screen === "talk" && taskOnScreen(state, local)?.task_id === task.task_id)
    ui.selectTask(task.project_id, task.task_id);
}

export function taskOnScreen(state: HostState, ui: Pick<UiState, "tab" | "stacks" | "projectId" | "taskByProject" | "taskList">): Task | null {
  const front = ui.stacks[ui.tab].at(-1);
  if (!front) return null;
  if ("taskId" in front) return front.taskId ? state.tasks[front.taskId] ?? null : null;
  if (front.screen === "desktop") return runningTasks(state).find((t) => t.target_id === front.targetId) ?? null;
  if (front.screen !== "talk" || ui.taskList) return null;
  const project = talkProject(state, ui.projectId);
  return project ? talkTask(state, project.project_id, ui.taskByProject[project.project_id]) : null;
}

export function useTaskOnScreen(): Task | null {
  const tab = useUi((s) => s.tab);
  const stacks = useUi((s) => s.stacks);
  const projectId = useUi((s) => s.projectId);
  const taskByProject = useUi((s) => s.taskByProject);
  const taskList = useUi((s) => s.taskList);
  return useHost((s) => taskOnScreen(s, { tab, stacks, projectId, taskByProject, taskList }));
}
