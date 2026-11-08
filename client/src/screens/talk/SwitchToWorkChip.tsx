// SPDX-License-Identifier: Apache-2.0
//
// Switch to Work in one tap: a running Plan or Ask task, or one whose Ask
// turn has ended, carries on in Work, and a running chat becomes a task,
// without waiting for its turn to end. The computer stops the turn cleanly
// and the same agent session carries on with what it learned. While another
// task changes the project, it waits its turn in Next up.
//
// A chat whose turn has ended still opens "Turn into a task" (IntoTask) to
// say what to do, and a finished plan has "Carry out this plan".

import { Hammer } from "lucide-react";
import { useState } from "react";
import { shallowEqual, useApp, useHost, useLink } from "../../app/context";
import { Chip } from "../../design/Chip";
import { toast } from "../../design/Toast";
import type { Task } from "../../protocol/types";
import { canSwitch, switchesToWork } from "./approvalWays";
import { otherHolder, switchToWork } from "./switchToWork";

const RUNNING = new Set<Task["state"]>(["dispatching", "working", "awaiting_decision"]);

/** Whether `task` offers the one-tap switch now. */
export function offersSwitch(task: Task | null | undefined, switches: boolean): task is Task {
  if (!switches || !canSwitch(task)) return false;
  if (RUNNING.has(task.state)) return true;
  // An ended chat has Turn into a task, and a finished plan Carry out this plan.
  return !task.conversation && task.mode === "ask" && (task.state === "completed" || task.state === "interrupted" || task.state === "failed");
}

export function switchLabel(task: Task): string {
  return task.conversation ? "Turn into a task" : "Switch to Work";
}

/** The chip beside the composer's other choices. */
export function SwitchToWorkChip({ task, className = "composer__chip composer__opt composer__switch" }: { task: Task; className?: string }) {
  const { session, profile } = useApp();
  const switches = useHost((s) => switchesToWork(s.capabilities));
  const canWork = useHost((s) => s.grants?.tasks === true);
  const holder = useHost((s) => otherHolder(s, task.project_id, task.task_id), shallowEqual);
  const project = useHost((s) => s.projects[task.project_id]?.name ?? "the project");
  const online = useLink().state === "online";
  const [busy, setBusy] = useState(false);
  if (!offersSwitch(task, switches)) return null;
  const go = async () => {
    setBusy(true);
    try {
      await switchToWork(session, task, null);
      if (holder) toast(`Switched to Work. It waits in Next up while ${holder.title ? `“${holder.title}”` : "another task"} changes ${project}.`);
    } catch (e) {
      const message = e && typeof e === "object" && "error" in e ? (e as { error: { message: string } }).error.message
        : `${profile.name} didn’t switch it.`;
      toast(message, { tone: "stop" });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Chip icon={Hammer} className={className} disabled={busy || !online || !canWork} onClick={() => void go()}
      title={task.conversation ? "Becomes a task in Work mode, in the same session" : "Carries on in Work mode, in the same session"}>
      {switchLabel(task)}
    </Chip>
  );
}
