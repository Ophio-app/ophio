// SPDX-License-Identifier: Apache-2.0
//
// Summarize: one tap asks a stopped or finished task's own agent, in its own
// session and with its own model, what happened. The computer runs that one
// turn read-only, so the answer changes nothing and needs no writer, and the
// task is left as it was.

import type { HarnessInfo, Run, Task } from "../../protocol/types";
import type { SendRequest } from "./send";

/** Whether the computer can ask a task one read-only question. */
export const READ_ONLY_TURN_FEATURE = "read_only_turn_v1";

export const SUMMARY_PROMPT =
  "Summarize what happened in this task: what you were asked, what you did, what changed (files, commands, checks and their results), what failed or is unfinished, and what you would do next. Keep it short, and say where you are unsure. Don’t change anything.";

/**
 * Whether this task's agent can be asked: it has stopped or finished, the
 * session of its current run was kept and its agent can resume it. A task that drives a
 * computer or answers on GitHub isn't asked, as its turns act beyond Talk.
 */
export function summarizable(task: Task, run: Run | null, harness: HarnessInfo | null): boolean {
  return (task.state === "completed" || task.state === "failed" || task.state === "interrupted")
    && task.kind !== "computer_use" && !task.github
    && run?.task_id === task.task_id && run.run_id === task.current_run && !!run.native_session
    && harness?.capabilities.start_run.state === "available"
    && harness.capabilities.resume_session.state === "available";
}

export function summaryRequest(projectId: string, task: Task): SendRequest {
  return {
    projectId,
    draftTaskId: task.task_id,
    text: SUMMARY_PROMPT,
    annotationId: null,
    label: "Summarize",
    expectedRevision: task.revision,
    readOnly: true,
    build: () => ({ command: "resume_task", task_id: task.task_id, text: SUMMARY_PROMPT, read_only: true }),
  };
}
