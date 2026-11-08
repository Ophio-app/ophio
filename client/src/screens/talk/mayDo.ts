// SPDX-License-Identifier: Apache-2.0
//
// Whether something is a chat or a task, and what it may do, in one short
// line worded the same way everywhere: at the top of its screen and on its
// row in Tasks. A chat is governed by its access alone, never by the mode
// the computer runs it in; a task by its mode, unless it was a chat turned
// into a task, which is Work and keeps the chat's access.
//
//   Chat, can read ophio and use the internet
//   Chat, can change ophio and notes, and use the internet
//   Chat, can use the whole computer, use the internet, and reach your own network
//   Task in Plan mode, changes nothing
//   Task in Work mode, can change ophio

import type { ConversationAccess, Project, Task } from "../../protocol/types";
import { heldAccess, normalizedAccess, reachesOwnNetwork } from "./access";

export type MayDoTask = Pick<Task, "conversation" | "mode" | "access" | "project_id">;

/** A chat, as opposed to a task. */
export const isChat = (task: Pick<Task, "conversation">): boolean => !!task.conversation;

/** Whether what the agent may reach is its access, chosen in the access bar:
 * a chat's, or that of a task a chat was turned into. */
export const governedByAccess = (task: Pick<Task, "conversation" | "access">): boolean => !!task.conversation || !!task.access;

/** A project as the line names it: Home is the person's home folder. */
const placeName = (p: Project): string => (p.home ? "your home folder" : p.name);

const list = (items: string[]): string => new Intl.ListFormat("en", { type: "conjunction" }).format(items);

/** Clauses as one list; a clause that names several projects keeps its own
 * "and", so the last clause then takes a comma, as in "change ophio and
 * notes, and use the internet". */
function clauses(items: string[]): string {
  if (items.length !== 2 || !items.some((c) => c.includes(" and "))) return list(items);
  return `${items[0]}, and ${items[1]}`;
}

const MODE_WORD = { ask: "Ask", plan: "Plan", work: "Work" } as const;

/** What `access` lets the agent do, as "can …" clauses. Its own project may
 * always be read; projects this computer no longer lists are left out. */
function reach(access: ConversationAccess, projectId: string, projects: Record<string, Project | undefined>): string[] {
  const own = projects[projectId];
  const names = (ids: string[]) => [...new Set(ids.flatMap((id) => {
    const p = projects[id];
    return p ? [placeName(p)] : id === projectId ? ["its project"] : [];
  }))];
  const out: string[] = [];
  if (access.whole_write) out.push("use the whole computer");
  else {
    const write = names(access.write);
    if (write.length) out.push(`change ${list(write)}`);
    if (access.whole_read) out.push("read the whole computer");
    else {
      const read = names([projectId, ...access.read]).filter((n) => !write.includes(n));
      if (read.length) out.push(`read ${list(read)}`);
    }
  }
  if (access.internet) out.push("use the internet");
  if (reachesOwnNetwork(access)) out.push("reach your own network");
  // Nothing at all to say still says it can read its own project.
  return out.length ? out : [`read ${own ? placeName(own) : "its project"}`];
}

/**
 * The one line saying what `task` is and what it may do. `held`: whether the
 * computer holds a chat to its access (`accessHeld`); where it can't, a chat
 * reads its own project and nothing else. `projects` names the projects.
 */
export function mayDo(task: MayDoTask, held: boolean, projects: Record<string, Project | undefined>): string {
  if (isChat(task)) return `Chat, can ${clauses(reach(heldAccess(task, held), task.project_id, projects))}`;
  const mode = task.mode ?? "work";
  // A task held to an access (a chat turned into a task, or one holding an
  // outsider's content) says its real mode and what that access really
  // lets it change, as the computer keeps it.
  if (task.access) return `Task in ${MODE_WORD[mode]} mode, can ${clauses(reach(normalizedAccess(task.access), task.project_id, projects))}`;
  if (mode !== "work") return `Task in ${MODE_WORD[mode]} mode, changes nothing`;
  const own = projects[task.project_id];
  return `Task in Work mode, can change ${own ? placeName(own) : "its project"}`;
}
