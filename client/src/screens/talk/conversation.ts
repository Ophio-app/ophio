// SPDX-License-Identifier: Apache-2.0
//
// Talk holds conversations: a long-running Ask task with one agent, in which
// the person can change model or effort between messages, and which they can
// turn into a task once they want something done. Work holds the tasks.

import type { AgentChoice } from "../../app/ui";
import type { Tone } from "../../design/Chip";
import type { Command, ConversationAccess, HarnessInfo, QueueView, Run, Task, TaskStart } from "../../protocol/types";
import { createTaskCommand } from "./intent";
import type { SendRequest } from "./send";
import { taskRowBadge } from "./TaskBar";

/** Whether the computer can hold a conversation and carry it into Work. */
export const CONVERSATIONS_FEATURE = "conversations_v1";
export const CONVERSATIONS_UPDATE = "Update Øphio on this computer to hold conversations in Chat.";

/** The draft a new conversation is written in, apart from a new task's. */
export const NEW_CONVERSATION = "conversation";

export const isConversation = (t: Task): boolean => !!t.conversation;

/**
 * A conversation that has replied, or whose reply was stopped, is just there
 * between messages, so it shows no state, and one archived is in Archived; a
 * question for the person or a failure says so.
 */
export function conversationAtRest(t: Task, reason: string | null): boolean {
  return !!t.conversation && (t.state === "completed" || t.state === "interrupted" || t.state === "archived") && !reason;
}

/** How a conversation stands, where that's worth a word: nothing between
 * messages, Replying… while a reply is under way, and otherwise what went
 * wrong or what it waits for. */
export function conversationBadge(t: Task, run: Run | undefined, queue: QueueView | undefined, reason: string | null): { label: string; tone: Tone } | null {
  if (conversationAtRest(t, reason)) return null;
  if (!reason && (t.state === "accepted" || t.state === "dispatching" || t.state === "working")) return { label: "Replying…", tone: "accent" };
  return taskRowBadge(t, run, queue, reason);
}

/** A conversation starts at once, in Ask, with the person's first message,
 * or from an agent or skill. */
export function startConversation(projectId: string, text: string, agent: AgentChoice, from: { start?: TaskStart; access?: ConversationAccess | null } = {}): Command {
  return createTaskCommand({ projectId, prompt: text, title: null, agent, mode: "ask", placement: { mode: "start_now" }, mission: null, bundle: null, conversation: true, start: from.start, access: from.access });
}

/** The first message of a conversation started from an agent or skill: it
 * hands the conversation to what was chosen. */
export const startWith = (name: string): string => `Start with ${name}.`;

/** The model a conversation's replies use, as the agent reports models. */
export function currentModel(task: Task, h: HarnessInfo | undefined): string | null {
  return task.model ?? h?.models.find((m) => m.is_default)?.id ?? null;
}

/**
 * What the next message changes, as resume_task's model and effort: nothing
 * when the choice is the conversation's own. A model sent without an effort
 * runs at that model's default, so a choice of the default effort names the
 * model too.
 */
export function turnModel(task: Task, h: HarnessInfo | undefined, pick: AgentChoice | null | undefined): { model?: string; effort?: string } {
  if (!pick || pick.harness !== task.harness) return {};
  const model = pick.model ?? currentModel(task, h);
  if (model !== currentModel(task, h)) return model ? { model, ...(pick.effort ? { effort: pick.effort } : {}) } : {};
  if (pick.effort === task.effort) return {};
  return pick.effort ? { effort: pick.effort } : model ? { model } : {};
}

/** The choice the composer's chip shows: what the next message will use. */
export function nextChoice(task: Task, pick: AgentChoice | null | undefined): AgentChoice {
  return pick && pick.harness === task.harness ? pick : { harness: task.harness, model: task.model, effort: task.effort };
}

export const CONVERSATION_SUMMARY_PROMPT =
  "Sum up this conversation so far: what we discussed, what was decided, and the open questions. Keep it short, and say where you are unsure. Don’t change anything.";

/** Ask for a summary of the conversation, read-only, as Summarize asks a task. */
export function conversationSummary(projectId: string, task: Task): SendRequest {
  return {
    projectId,
    draftTaskId: task.task_id,
    text: CONVERSATION_SUMMARY_PROMPT,
    annotationId: null,
    label: "Sum up",
    expectedRevision: task.revision,
    readOnly: true,
    build: () => ({ command: "resume_task", task_id: task.task_id, text: CONVERSATION_SUMMARY_PROMPT, read_only: true }),
  };
}

/** Carry the conversation on in Work: the same agent and session, now able to change files. */
export function intoTask(projectId: string, task: Task, text: string): SendRequest {
  const body = text.trim();
  return {
    projectId,
    draftTaskId: task.task_id,
    text,
    annotationId: null,
    label: "Turn into a task",
    expectedRevision: task.revision,
    asTask: true,
    build: () => ({ command: "resume_task", task_id: task.task_id, mode: "work", text: body }),
  };
}
