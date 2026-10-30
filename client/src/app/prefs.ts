// SPDX-License-Identifier: Apache-2.0
//
// Phone preferences that do not belong to any one computer: appearance,
// launch defaults, warnings and the last computer used. Appearance applies
// before the first frame.

import { Store } from "../state/store";
import type { HarnessKind } from "../protocol/types";

export type Theme = "system" | "light" | "dark";

export interface Prefs {
  theme: Theme;
  /** Reduce motion regardless of the system setting. */
  reduceMotion: boolean;
  lastProfileId: string | null;
  claudeBypassPermissions: boolean;
  codexBypassPermissions: boolean;
  /** The agent new work goes to where a project has no default of its own;
   * null for the first one that's ready. */
  defaultAgent?: HarnessKind | null;
  /** Tilting the phone pans a zoomed display. */
  tiltPan?: boolean;
  /** Every warning is off: actions go ahead without asking first. */
  warningsOff?: boolean;
  /** Warnings turned off one at a time with “Don’t show me again”. */
  hiddenWarnings?: string[];
  /** Screen names given on this device, by computer and connector. */
  screenNames?: Record<string, string>;
}

const KEY = "prefs";
const initial: Prefs = { theme: "system", reduceMotion: false, lastProfileId: null, claudeBypassPermissions: true, codexBypassPermissions: true };

function load(): Prefs {
  try {
    const raw = localStorage.getItem(KEY);
    const saved = raw ? JSON.parse(raw) as Partial<Prefs> : {};
    return {
      ...initial, ...saved,
      claudeBypassPermissions: typeof saved?.claudeBypassPermissions === "boolean" ? saved.claudeBypassPermissions : true,
      codexBypassPermissions: typeof saved?.codexBypassPermissions === "boolean" ? saved.codexBypassPermissions : true,
      defaultAgent: typeof saved?.defaultAgent === "string" ? saved.defaultAgent : null,
      tiltPan: saved?.tiltPan === true,
      warningsOff: saved?.warningsOff === true,
      hiddenWarnings: Array.isArray(saved?.hiddenWarnings) ? saved.hiddenWarnings.filter((id): id is string => typeof id === "string") : [],
      screenNames: saved?.screenNames && typeof saved.screenNames === "object" && !Array.isArray(saved.screenNames)
        ? Object.fromEntries(Object.entries(saved.screenNames).filter((e): e is [string, string] => typeof e[1] === "string"))
        : {},
    };
  } catch {
    return initial;
  }
}

export const prefs = new Store<Prefs>(load());

prefs.subscribe(() => {
  try {
    localStorage.setItem(KEY, JSON.stringify(prefs.get()));
  } catch {
    // Unavailable storage: preferences last for this run only.
  }
  apply();
});

const dark = typeof matchMedia === "function" ? matchMedia("(prefers-color-scheme: dark)") : null;
const reduce = typeof matchMedia === "function" ? matchMedia("(prefers-reduced-motion: reduce)") : null;

/** Both CSS and imperative scrolling honor the app and system preferences. */
export function motionReduced(): boolean {
  return prefs.get().reduceMotion || (reduce?.matches ?? false);
}

/** Call `listener` whenever either preference that reduces motion may have
    changed; returns a function that stops. */
export function watchMotion(listener: () => void): () => void {
  const stop = prefs.subscribe(listener);
  reduce?.addEventListener("change", listener);
  return () => {
    stop();
    reduce?.removeEventListener("change", listener);
  };
}

/** Reflect preferences on the document root. */
export function apply(): void {
  const p = prefs.get();
  const root = document.documentElement;
  const isDark = p.theme === "dark" || (p.theme === "system" && (dark?.matches ?? false));
  root.dataset.theme = isDark ? "dark" : "light";
  root.dataset.reduceMotion = String(motionReduced());
  const bg = getComputedStyle(root).getPropertyValue("--bg").trim();
  if (bg) {
    for (const m of document.querySelectorAll<HTMLMetaElement>('meta[name="theme-color"]')) m.content = bg;
  }
}

dark?.addEventListener("change", apply);
reduce?.addEventListener("change", apply);
