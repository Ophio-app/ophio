// SPDX-License-Identifier: Apache-2.0
//
// Switch to Work, as sent from a request card or the one-tap control: the
// task shows in Work at once, and goes back if the computer refuses.

import { commandResult } from "../../host/commandResult";
import { settle } from "../../host/expected";
import type { HostSession } from "../../host/session";
import type { Task } from "../../protocol/types";
import type { HostState } from "../../state/projection";
import { asSwitched, switchCommand, type Holder } from "./approvalWays";

/** Who else holds `projectId`'s writer slot, as `task` would wait for. */
export function otherHolder(s: HostState, projectId: string, taskId: string): Holder | null {
  const writer = s.projects[projectId]?.active_writer;
  const run = writer ? s.runs[writer] : undefined;
  if (!writer || run?.task_id === taskId) return null;
  return { title: (run && s.tasks[run.task_id]?.title) ?? null };
}

/**
 * Switches `task` to Work, or turns a chat into a task, in one tap: it shows
 * in Work at once, and goes back if the computer refuses. `approvalId`: the
 * request it allows by switching.
 */
export async function switchToWork(session: HostSession, task: Task, approvalId: string | null, text?: string | null): Promise<void> {
  const id = task.task_id;
  const drop = session.state.expect((s) => {
    const current = s.tasks[id];
    return current ? { ...s, tasks: { ...s.tasks, [id]: asSwitched(current) } } : s;
  });
  try {
    const label = task.conversation ? `Turn “${task.title}” into a task` : `Switch “${task.title}” to Work`;
    await commandResult(session, await session.command(switchCommand(id, approvalId, text), { label }));
  } catch (e) {
    drop();
    throw e;
  }
  settle(session.state, drop, (s) => s.tasks[id]?.mode === "work" && !s.tasks[id]?.conversation);
}
