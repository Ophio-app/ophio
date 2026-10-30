// SPDX-License-Identifier: Apache-2.0
//
// One order for All: things holding work up, today's things, then the rest.
// Project and item ids break ties, so unrelated host updates never shuffle it.

import { checklistUrgency, orderedChecklistItems, today } from "../../app/checklist";
import type { Checklist, ChecklistItem } from "../../protocol/types";

export interface TodoOrder {
  id: string;
  projectId: string;
  tier: number;
  priority?: ChecklistItem["priority"];
  due?: string;
  done?: boolean;
  /** When it was done, so the latest done comes first. */
  doneAt?: number;
  order?: number;
}

export function checklistOrder(list: Checklist, day = today()): (TodoOrder & { item: ChecklistItem })[] {
  const ranks = checklistUrgency(list, day);
  return orderedChecklistItems(list, day).map((item, order) => ({
    id: item.item_id, projectId: list.project_id, item, order,
    ...ranks.get(item.item_id)!, done: item.done_at !== undefined, doneAt: item.done_at,
  }));
}

const priority = { blocker: 0, high: 1, medium: 2, low: 3 };
/** Finished things go last, the latest first; today's things go by their day
 * before priority. */
export function compareTodo(a: TodoOrder, b: TodoOrder): number {
  const day = (a.due ?? "9999").localeCompare(b.due ?? "9999");
  return Number(!!a.done) - Number(!!b.done)
    || (b.doneAt ?? 0) - (a.doneAt ?? 0)
    || a.tier - b.tier
    || (a.tier === 1 ? day : 0)
    || priority[a.priority ?? "medium"] - priority[b.priority ?? "medium"]
    || day || a.projectId.localeCompare(b.projectId) || (a.order ?? 0) - (b.order ?? 0) || a.id.localeCompare(b.id);
}
