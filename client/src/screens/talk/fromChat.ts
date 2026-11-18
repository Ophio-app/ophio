// SPDX-License-Identifier: Apache-2.0
//
// Where a task came from, when it came from a chat: a card's Start, or Turn
// into a task. Tasks lists it with the chat's name, and the task's heading
// has the way back to the chat.

import { useHost } from "../../app/context";
import type { Task } from "../../protocol/types";

/** The chat `task` was started from, by id; null when it wasn't. */
export const fromChatId = (task: Task): string | null => task.origin?.chat ?? task.from_chat ?? null;

/** The chat's name, or null once the computer no longer has it. */
export function useFromChat(task: Task): { chatId: string; title: string | null; projectId: string | null } | null {
  const id = fromChatId(task);
  const chat = useHost((s) => (id ? s.tasks[id] : undefined));
  if (!id) return null;
  return { chatId: id, title: chat?.title ?? null, projectId: chat?.project_id ?? null };
}

/** "From chat “Upload progress”", or "From a chat" once it is gone. */
export const fromChatLine = (title: string | null): string => (title ? `From chat “${title}”` : "From a chat");
