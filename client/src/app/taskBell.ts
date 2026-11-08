// SPDX-License-Identifier: Apache-2.0
//
// "Tell me when it's done": a task's own bell. Finished tasks wait in the app
// by default; with the bell on, this task's finish, failure or reply alerts
// on every device paired with the computer, whatever each one's choice. The
// computer keeps it on the task (`set_task_notify`), so it shows on the tap
// and goes back with a toast if the computer refuses. Only the person turns
// it: the computer refuses it from an agent. Kept apart from the
// screens: the shell's offer after starting a task uses it too.

import { useCommandControl } from "./commandControl";
import { describeError, useApp, useHost, useLink } from "./context";
import { ALERT_TYPES_FEATURE } from "../platform/alertTypes";
import { notificationAccess } from "../platform/notifications";
import type { Task } from "../protocol/types";

/** The task's bell, and a way to turn it, on a computer that keeps it. */
export function useTaskBell(task: Task) {
  const { session, profile, platform } = useApp();
  const typed = useHost((s) => s.capabilities?.features.includes(ALERT_TYPES_FEATURE) ?? false);
  // As useMay: false only once the computer has said this device may not.
  // (Not imported: the shell loads this at start, and NotAllowed is a screen's.)
  const allowed = useHost((s) => s.grants?.tasks !== false);
  const online = useLink().state === "online";
  const noun = task.conversation ? "conversation" : "task";
  const control = useCommandControl<boolean>({
    kind: "instant",
    value: !!task.notify_done,
    describe: (e) => describeError(e, profile.name),
    send: (on, note) => session.command({ command: "set_task_notify", task_id: task.task_id, on }, { label: note.label }),
  });
  const set = (on: boolean) => {
    // Turning it on is asking for a notification: ask Android for leave too.
    if (on && platform.notify) {
      const access = notificationAccess(platform);
      if (access.get().permission === "promptable") void access.allow();
    }
    return control.set(on, {
      label: on ? "Tell me when it’s done" : "Stop telling me when it’s done",
      what: `whether this ${noun} tells you when it’s done`,
      said: on ? `You’ll be told when this ${noun} is done.` : `This ${noun} waits in the app when it’s done.`,
    });
  };
  return { shown: typed && task.state !== "archived", on: control.value, can: allowed && online, set, noun };
}

