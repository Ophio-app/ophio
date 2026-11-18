// SPDX-License-Identifier: Apache-2.0
//
// The chat a task came from, one tap back to it: on the task's heading and
// on its receipt. Plain words once the chat is gone.

import { useApp } from "../../app/context";
import type { Task } from "../../protocol/types";
import { fromChatLine, useFromChat } from "./fromChat";

/** `short` shows just "From chat" where a heading has no room for the
 * chat's name; the link still names it to a screen reader. */
export function FromChatLink({ task, className, short = false }: { task: Task; className: string; short?: boolean }) {
  const { ui } = useApp();
  const from = useFromChat(task);
  if (!from) return null;
  const { chatId, title, projectId } = from;
  if (!title || !projectId) return <span className={className}>{fromChatLine(null)}</span>;
  return (
    <button type="button" className={className} onClick={() => ui.openTask(projectId, chatId)}
      aria-label={`${fromChatLine(title)}. Open the chat`}>
      <span className={`${className}-text`}>{short ? "From chat" : fromChatLine(title)}</span>
    </button>
  );
}
