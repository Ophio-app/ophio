// SPDX-License-Identifier: Apache-2.0
import { useEffect } from "react";
import { useHandledOnScreen } from "../platform/handled";
import { viewingTask } from "../platform/notifications";
import { useRoute } from "../shell/Screen";
import { shallowEqual, useApp, useHost } from "./context";

/** While the task or conversation `taskId` is open on the screen, its alerts
 * leave the phone's shade, answered or not, and count as handled on the
 * person's other devices too. */
export function useViewingTask(taskId: string | null | undefined): void {
  const { profile, platform } = useApp();
  const { active } = useRoute();
  useEffect(
    () => (active && taskId ? viewingTask(profile.profileId, taskId) : undefined),
    [active, taskId, profile.profileId],
  );
  const open = useHost((s) => taskId ? Object.values(s.attention).flatMap((a) => a.task_id === taskId && a.resolved_at === null ? [a.attention_id] : []).sort() : NONE, shallowEqual);
  useHandledOnScreen(platform, profile.profileId, open, active);
}
const NONE: string[] = [];
