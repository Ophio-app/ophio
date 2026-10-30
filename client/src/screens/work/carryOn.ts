// SPDX-License-Identifier: Apache-2.0
//
// Carry on and Try again go to the task as they are tapped. A task the
// project's writer holds can't start yet, so its message waits in Next up
// and the task carries on once the project is free.

import type { App } from "../../app/context";
import type { Task } from "../../protocol/types";
import type { HostState } from "../../state/projection";
import { writerHold, type WriterHold } from "../talk/intent";
import { send, type SendOutcome } from "../talk/send";

/** What holds `task` back from carrying on now, if anything. Only work
 * waits for the project's writer; a read-only task can start beside it. */
export function carryOnHold(s: Pick<HostState, "projects" | "runs" | "tasks" | "control">, task: Task): WriterHold | null {
  return (task.mode ?? "work") === "work" ? writerHold(s.projects[task.project_id]?.active_writer, s) : null;
}

/** Sends `text` as the task's next turn, now or through Next up when
 * `held`. It goes through the task's own draft, so an answer that never
 * comes is checked there rather than sent twice. */
export function carryOn(app: App, task: Task, text: string, held: boolean): Promise<SendOutcome> {
  return send(app, {
    projectId: task.project_id,
    draftTaskId: task.task_id,
    text,
    annotationId: null,
    label: held ? "Put in Next up" : "Continue",
    expectedRevision: task.revision,
    build: () => held
      ? { command: "queue_continuation", task_id: task.task_id, text, authorization: "auto_start" }
      : { command: "resume_task", task_id: task.task_id, text },
  }, () => {});
}
