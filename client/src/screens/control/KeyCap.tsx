// SPDX-License-Identifier: Apache-2.0
import { ArrowDown, ArrowLeft, ArrowRight, ArrowUp, type LucideIcon } from "lucide-react";

/**
 * Arrow glyphs come from different fallback fonts on some phones and render
 * at different sizes, so arrow keys draw one icon set with a spoken name.
 */
const ARROWS: Record<string, { icon: LucideIcon; label: string }> = {
  "←": { icon: ArrowLeft, label: "Left arrow" },
  "↑": { icon: ArrowUp, label: "Up arrow" },
  "↓": { icon: ArrowDown, label: "Down arrow" },
  "→": { icon: ArrowRight, label: "Right arrow" },
};

/** Button props for one key: an icon with an accessible name, or its text. */
export function keyCap(key: string): { icon?: LucideIcon; "aria-label"?: string; children: string | null } {
  const arrow = ARROWS[key];
  return arrow ? { icon: arrow.icon, "aria-label": arrow.label, children: null } : { children: key };
}
