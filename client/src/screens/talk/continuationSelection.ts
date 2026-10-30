// SPDX-License-Identifier: Apache-2.0
import type { UiStore } from "../../app/ui";
import { Store } from "../../state/store";

// A receipt can request Continue while Talk is still mounted on that task.
// Keep that explicit choice separate from the person's draft text.
const selections = new WeakMap<UiStore, Store<{ taskId: string } | null>>();
export function continuationSelection(ui: UiStore) {
  let selection = selections.get(ui);
  if (!selection) { selection = new Store<{ taskId: string } | null>(null); selections.set(ui, selection); }
  return selection;
}

// Inline plan actions use the composer's same review/error flow. `send`
// starts it from Review unless something there needs choosing.
type PlanRequest = { taskId: string; send?: boolean };
const planRequests = new WeakMap<UiStore, Store<PlanRequest | null>>();
export function planReviewSelection(ui: UiStore) {
  let selection = planRequests.get(ui);
  if (!selection) { selection = new Store<PlanRequest | null>(null); planRequests.set(ui, selection); }
  return selection;
}
