// SPDX-License-Identifier: Apache-2.0

import type { Task } from "../protocol/types";

export const CHECKS_FEATURE = "checks_v1";
export const CHECKS_UPDATE = "Update this computer to use checks in the Library.";
export const CHECKS_QUERIES = ["checks_list", "checks_history", "task_checks", "check_job"];
export const CHECKS_COMMANDS = ["checks_save", "checks_ask", "checks_apply_draft", "checks_discard_draft", "checks_run_now", "checks_stop_job"];

/** Check progress is the task's status even after its agent's run has ended. */
export function checkingLabel(task: Pick<Task, "state" | "checking">): string | null {
  if (task.state !== "working" || !task.checking) return null;
  const { done, total } = task.checking;
  return `Checking…${total > 0 ? ` · ${Math.min(Math.max(0, done), total)} of ${total}` : ""}`;
}
