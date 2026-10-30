// SPDX-License-Identifier: Apache-2.0
import { useEffect } from "react";
import { viewingTask } from "../platform/notifications";
import { useRoute } from "../shell/Screen";
import { useApp } from "./context";

/** While the task or conversation `taskId` is open on the screen, its alerts
 * leave the phone's shade, answered or not. */
export function useViewingTask(taskId: string | null | undefined): void {
  const { profile } = useApp();
  const { active } = useRoute();
  useEffect(
    () => (active && taskId ? viewingTask(profile.profileId, taskId) : undefined),
    [active, taskId, profile.profileId],
  );
}
