// SPDX-License-Identifier: Apache-2.0
//
// Destinations opened for one purpose: a task, a file, a review, a setting.
// Opened over a tab, one takes the whole screen like a modal: it closes with
// an X, and the tabs and the running-tasks rail step aside until it does.
// Browsing lists and the control tools keep the full shell.

import { createContext, useContext, useLayoutEffect } from "react";
import type { Route } from "../app/ui";
import type { Store } from "../state/store";

const FOCUSED: ReadonlySet<Route["screen"]> = new Set<Route["screen"]>([
  "receipt",
  "review",
  "annotate",
  "attention",
  "file",
  "conflict",
  "instructions",
  "reusable",
  "computer_use_review",
  "library_recovery",
  "settings",
  "accounts",
  "agent_logins",
  "connections",
  "github",
  "github_list",
  "github_new",
  "findings",
  "contribution",
  "contribution_rules",
  "findings_new",
  "checklist",
  "stages",
  "transfers",
  "recently_deleted",
]);

/** A single-purpose destination, which never applies to a tab's root. */
export function isFocused(route: Route | undefined, depth: number): boolean {
  return depth > 0 && !!route && FOCUSED.has(route.screen);
}

/** Whether the shell's running-tasks rail is open, shared by the rail and
 * the count a focused screen shows in its bar while the rail is tucked. */
export const RailOpen = createContext<Store<boolean> | null>(null);

export function useRailOpen(): Store<boolean> | null {
  return useContext(RailOpen);
}

/** Whether a display fills the screen: the tabs, the bar and the rail step
 * aside, and the display's own overlay keeps Stop within reach. */
export const Immersive = createContext<Store<boolean> | null>(null);

export function useImmersive(on: boolean): void {
  const shown = useContext(Immersive);
  useLayoutEffect(() => {
    if (!shown || !on) return;
    shown.set(true);
    return () => shown.set(false);
  }, [shown, on]);
}

/** Whether Talk's chat header is up, and which task it shows. While it is, its
 * activity row sums up the rail, and an open task is the whole screen. A Talk
 * still loading, or without a project, keeps the full shell. */
export const ChatShown = createContext<Store<{ taskId: string | null } | null> | null>(null);

export function useChatShown(taskId: string | null): void {
  const shown = useContext(ChatShown);
  useLayoutEffect(() => {
    if (!shown) return;
    shown.set({ taskId });
    return () => shown.set(null);
  }, [shown, taskId]);
}
