// SPDX-License-Identifier: Apache-2.0
import type { UiStore } from "../../app/ui";
import { continuationSelection } from "./continuationSelection";

/** Both preview and receipt retries end at the composer for a fresh review. */
export function openRetryDraft(ui: UiStore, projectId: string, taskId: string | null): void {
  ui.openTask(projectId, taskId);
  if (taskId) continuationSelection(ui).set({ taskId });
}
