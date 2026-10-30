// SPDX-License-Identifier: Apache-2.0
//
// The person's answers to what an agent asked about a preview. They are
// written beside the page, kept on this device until they are sent, and go
// to the task as its next message.

import { choices, unpicked, type Picked } from "../../design/ApprovalQuestions";
import type { ApprovalQuestion, Task } from "../../protocol/types";
import { readLocal, writeLocal } from "../work/local";

export interface PreviewAnswers {
  picked: Picked[];
  /** The person finished answering; the answers wait for them to send. */
  submitted: boolean;
  /** When the answers went to the task. */
  sentAt: number | null;
}

/** Answers for every preview a task showed, by when it showed it. */
const key = (taskId: string) => `preview-answers.${taskId}`;

function saved(taskId: string): Record<string, PreviewAnswers> {
  const all = readLocal<unknown>(key(taskId), {});
  return all && typeof all === "object" ? (all as Record<string, PreviewAnswers>) : {};
}

const fits = (picked: unknown, questions: ApprovalQuestion[]): picked is Picked[] =>
  Array.isArray(picked) && picked.length === questions.length &&
  picked.every(p => p && typeof p === "object" && Array.isArray(p.labels) && p.labels.every((l: unknown) => typeof l === "string") && typeof p.own === "string");

export function readAnswers(taskId: string, at: number, questions: ApprovalQuestion[]): PreviewAnswers {
  const answers = saved(taskId)[at];
  return answers && fits(answers.picked, questions)
    ? { picked: answers.picked, submitted: answers.submitted === true, sentAt: typeof answers.sentAt === "number" ? answers.sentAt : null }
    : { picked: unpicked(questions), submitted: false, sentAt: null };
}

export function writeAnswers(taskId: string, at: number, answers: PreviewAnswers): boolean {
  return writeLocal(key(taskId), { ...saved(taskId), [at]: answers });
}

/** The task has answers the person finished and left to send later. */
export function unsentAnswers(taskId: string): boolean {
  return Object.values(saved(taskId)).some(a => a?.submitted === true && a.sentAt == null);
}

/** Whether any question has an answer yet. */
export const anyAnswered = (questions: ApprovalQuestion[], picked: Picked[]): boolean =>
  questions.some((q, i) => picked[i] && choices(q, picked[i]).length > 0);

/** The message the agent reads: each question as it asked it, then the answer. */
export function answerMessage(label: string, questions: ApprovalQuestion[], picked: Picked[]): string | null {
  if (!anyAnswered(questions, picked)) return null;
  const lines = questions.map((q, i) => {
    const said = picked[i] ? choices(q, picked[i]) : [];
    return `${q.question}\n→ ${said.length ? said.join(", ") : "(no answer)"}`;
  });
  return `My answers to your questions about the preview “${label}”:\n\n${lines.join("\n\n")}`;
}

/** How the answers reach the task: a task that is working reads them after
 * its current turn; one that has stopped waits in Next up to start again. */
export function answerRoute(task: Task | undefined): "next_turn" | "next_up" | null {
  if (!task) return null;
  if (["interrupted", "completed", "failed"].includes(task.state)) return "next_up";
  if (["dispatching", "working", "awaiting_decision"].includes(task.state)) return "next_turn";
  return null;
}
