// SPDX-License-Identifier: Apache-2.0
//
// While the header shows All computers, Tasks and To-Do show every project
// on every computer at once, until a project is chosen; the project picker's
// All projects goes back. Kept with the phone, not a computer, since it
// spans them, and remembered as All is.

import { useStore } from "../state/store";
import { useApp, useUi } from "./context";
import { prefs } from "./prefs";
import type { Tab } from "./ui";

/** Tasks and To-Do show every computer's projects, while All is chosen.
 * `tab` is the tab the asking screen belongs to: every visited tab stays
 * mounted, so each decides for itself and keeps its own view, scroll and
 * open sheet; without one, the tab on screen. */
export function useEveryProject(tab?: Tab | null): boolean {
  const { all } = useApp();
  const chosen = useStore(prefs, (p) => p.allProjects !== false);
  const shown = useUi((s) => s.tab);
  const own = tab ?? shown;
  return all === true && chosen && (own === "work" || own === "todo");
}

export function showEveryProject(on: boolean): void {
  prefs.set((p) => (p.allProjects === on ? p : { ...p, allProjects: on }));
}
