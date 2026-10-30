// SPDX-License-Identifier: Apache-2.0
//
// What a conversation's agent may reach: the projects it may read, those it
// may change and run commands in, or the whole computer, and the internet.
// The computer holds the agent to it once it says it can
// (`conversation_access`); until then a conversation reads its own project
// and nothing else, as Ask runs.

import type { ConversationAccess, HostCapabilities, Project, Task } from "../../protocol/types";

/** What a new conversation may reach until the person chooses otherwise. */
export function defaultAccess(projectId: string): ConversationAccess {
  return { read: [projectId], write: [], whole_read: false, whole_write: false, internet: true, local_network: false };
}

/** What a conversation reaches on a computer that can't hold it to a choice. */
export function askAccess(projectId: string): ConversationAccess {
  return { ...defaultAccess(projectId), internet: false };
}

/** Whether `access` lets the agent change anything. */
export const accessChanges = (a: ConversationAccess): boolean => a.whole_write || a.write.length > 0;

/** What a conversation's agent reaches now: what the person chose where the
 * computer holds it to that, else what Ask runs reach. */
export function heldAccess(task: Pick<Task, "access" | "project_id">, held: boolean): ConversationAccess {
  return task.access ?? (held ? defaultAccess(task.project_id) : askAccess(task.project_id));
}

/** Whether the computer holds a conversation to what the person chose. */
export const accessHeld = (caps: HostCapabilities | null | undefined): boolean => caps?.conversation_access?.state === "available";

/** Why a new conversation offers no choice of what it may reach, on a
 * computer that says it can't hold one to it; null where it can, or where it
 * predates saying so. */
export function unheldWords(caps: HostCapabilities | null | undefined): string | null {
  const a = caps?.conversation_access;
  if (!a || a.state === "available") return null;
  if (a.state === "unsupported" || a.state === "incompatible") return "This computer can’t limit what a chat may reach yet, so chats here can only read.";
  return "To choose what a chat may reach, this computer needs one more setup step. Run ophio doctor on the computer to see it.";
}

/** A project as the sentence names it: Home is the person's home folder. */
const placeName = (p: Project): string => (p.home ? "your home folder" : p.name);

const list = (names: string[]): string => new Intl.ListFormat("en", { type: "conjunction" }).format(names);

/**
 * What `access` lets the agent of a conversation in `here` do, in a
 * sentence or three. The conversation's own project may always be read.
 * Projects this computer no longer lists are left out.
 */
export function describeAccess(access: ConversationAccess, here: Project, projects: Record<string, Project | undefined>): string {
  const names = (ids: string[]) => [...new Set(ids.map((id) => (id === here.project_id ? here : projects[id])).filter((p): p is Project => !!p).map(placeName))];
  const write = access.whole_write ? [] : names(access.write);
  const read = names([here.project_id, ...access.read]).filter((n) => !write.includes(n));
  const reach = access.whole_write ? "The agent can read and change anything on this computer, and run any command."
    : access.whole_read && write.length ? `The agent can read anything on this computer, and change ${list(write)} and run commands there.`
    : access.whole_read ? "The agent can read anything on this computer but can’t change anything."
    : write.length ? `The agent can change ${list(write)} and run commands there${read.length ? `, and read ${list(read)}` : ""}.`
    : `The agent can read ${list(read)} but can’t change anything.`;
  const internet = !access.internet ? "It can’t reach the internet."
    : reachesOwnNetwork(access) ? "It can look things up on the internet and reach your own network, such as your tailnet."
    : "It can look things up on the internet.";
  const changes = access.whole_write || write.length > 0;
  return `${reach} ${internet}${changes ? "" : " Turn it into a task when you want something done."}`;
}

/**
 * Whether the agent reaches the person's own network (their LAN or tailnet):
 * when let, or when nothing holds it in at all, as with the whole computer to
 * change and the internet (`access_reach`).
 */
export const reachesOwnNetwork = (a: ConversationAccess): boolean => a.internet && (!!a.local_network || a.whole_write);

/**
 * What a conversation turned into a task may reach, as the computer keeps
 * it: the conversation's access, and its own project to change, which is
 * what it was turned into a task for.
 */
export function carriedAccess(access: ConversationAccess, projectId: string): ConversationAccess {
  return normalizedAccess({ ...access, write: [...access.write, projectId] });
}

/**
 * `access` as the computer keeps it (`ConversationAccess::normalized`): what
 * the agent may change it may read, changing the whole computer reads it, and
 * the person's own network goes only with the internet.
 */
export function normalizedAccess(a: ConversationAccess): ConversationAccess {
  const read = [...new Set([...a.read, ...a.write])].sort();
  const write = [...new Set(a.write)].sort();
  return { ...a, read, write, whole_read: a.whole_read || a.whole_write, local_network: a.internet && !!a.local_network };
}
