// SPDX-License-Identifier: Apache-2.0
import { Bell, BellRing } from "lucide-react";
import { useTaskBell } from "../../app/taskBell";
import { IconButton } from "../../design/Button";
import type { Task } from "../../protocol/types";

/** The bell in the task's header. */
export function TaskBell({ task }: { task: Task }) {
  const bell = useTaskBell(task);
  if (!bell.shown) return null;
  return <IconButton icon={bell.on ? BellRing : Bell} pressed={bell.on} disabled={!bell.can}
    className={bell.on ? "task-bell is-on" : "task-bell"}
    label="Tell me when it’s done"
    onClick={() => void bell.set(!bell.on)} />;
}
