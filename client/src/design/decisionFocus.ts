// SPDX-License-Identifier: Apache-2.0
//
// Where focus goes once a decision is accepted. The control that made it is
// gone or disabled by then, and focus would otherwise drop to the page.

/** Marks a request this device can still decide. */
export const OPEN_DECISION = "data-open-decision";

/**
 * Call before sending a decision; the returned function, called once it is
 * accepted, focuses the next request still waiting on the same screen or
 * sheet, else `fallback`: the decided request, or where the list it leaves
 * begins. The request may already have been redrawn as settled by then. Focus
 * the person has since put somewhere else stays there.
 */
export function focusAfterDecision(decided: HTMLElement | null, fallback: HTMLElement | null | undefined = decided): () => void {
  if (!decided) return () => {};
  const scope = decided.closest("dialog, .route") ?? document.body;
  return () => {
    const active = document.activeElement;
    if (active && active !== document.body && !decided.contains(active)) return;
    const open = [...scope.querySelectorAll<HTMLElement>(`[${OPEN_DECISION}]`)].filter((el) => el !== decided && !decided.contains(el));
    const after = decided.isConnected ? open.find((el) => decided.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_FOLLOWING) : undefined;
    const target = after ?? open[0] ?? (fallback?.isConnected ? fallback : undefined);
    if (!target) return;
    if (!target.hasAttribute("tabindex")) target.tabIndex = -1;
    target.focus();
  };
}
