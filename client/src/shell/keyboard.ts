// SPDX-License-Identifier: Apache-2.0
// Public layout hooks use the containing pane when there is one, and preserve
// whole-window behavior for screens rendered without a PaneFrame. The shared
// observer lives independently in viewport.ts, so PaneFrame does not depend
// on this higher-level facade (which itself reads the pane context).
import { usePane } from "./pane";
import { usePaneLayout } from "./viewport";
export { classifyLayout, useKeyboardOpen, usePaneLayout } from "./viewport";

/** The containing pane is short for its text size, or the legacy screen is. */
export function useTightLayout(): boolean {
  return usePaneLayout(usePane()).tight;
}

/** The containing pane is narrow for its text size, or the legacy screen is. */
export function useNarrowLayout(): boolean {
  return usePaneLayout(usePane()).narrow;
}

/** The containing pane's visible height is cramped for its text size. */
export function useCrampedLayout(): boolean {
  return usePaneLayout(usePane()).cramped;
}

/** The containing pane can put its pinned rows side by side. */
export function useSidewaysLayout(): boolean {
  return usePaneLayout(usePane()).sideways;
}

export function hideKeyboard(): void {
  (document.activeElement as HTMLElement | null)?.blur();
}
