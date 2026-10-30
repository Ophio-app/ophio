// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef } from "react";
import { useApp, useHost } from "../app/context";
import { toast } from "../design/Toast";
import { isActiveTask } from "../state/projection";

/**
 * Tells the owner, while the app is open, each time an agent used a login
 * they asked to hear about. Only uses heard live: a reconnect's replay does
 * not tell of them again, and nothing is sent when the app is closed.
 */
export function LoginUseNotices() {
  const { ui } = useApp();
  const use = useHost((s) => s.agentLoginUse);
  const task = useHost((s) => (use ? s.tasks[use.taskId] ?? null : null));
  // Whatever was already recorded when this mounted has been told of, or was never live.
  const told = useRef(use?.seq ?? null);
  useEffect(() => {
    if (!use || told.current === use.seq) return;
    told.current = use.seq;
    toast(task ? `${use.name} was used on ${use.site} for “${task.title}”` : `${use.name} was used on ${use.site}`, {
      action: task ? {
        label: "Open task",
        run: () => {
          if (isActiveTask(task)) ui.openTask(task.project_id, task.task_id);
          else ui.push({ screen: "receipt", taskId: task.task_id });
        },
      } : undefined,
    });
  }, [use, task, ui]);
  return null;
}
