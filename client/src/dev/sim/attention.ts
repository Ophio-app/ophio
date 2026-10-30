// SPDX-License-Identifier: Apache-2.0
import type { AttentionItem } from "../../protocol/types";

/** Review results stay visible without making a task need attention. */
export function blockingAttentionCount(items: AttentionItem[], taskId: string): number {
  return items.filter(a => a.task_id === taskId && a.resolved_at === null && a.kind !== "completed" && a.kind !== "failure").length;
}
