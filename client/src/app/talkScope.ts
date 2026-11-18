// SPDX-License-Identifier: Apache-2.0
// Chat's default list scope belongs to this computer, never the All computers view.
// It is worked out from whatever the phone holds, so the list keeps its scope
// while the phone reconnects (what it held is kept until the fresh copy lands).

import { useHost, useUi } from "./context";
import type { UiState } from "./ui";
import type { HostState } from "../state/projection";

export function talkAll(ui: Pick<UiState, "talkAll">, host: Pick<HostState, "projects" | "tasks">): boolean {
  if (ui.talkAll !== null) return ui.talkAll;
  const projects = new Set<string>();
  for (const task of Object.values(host.tasks)) {
    if (!task.conversation || !host.projects[task.project_id]) continue;
    projects.add(task.project_id);
    if (projects.size > 1) return true;
  }
  return false;
}

export function useTalkAll(): boolean {
  const choice = useUi((s) => s.talkAll);
  return useHost((s) => talkAll({ talkAll: choice }, s));
}
