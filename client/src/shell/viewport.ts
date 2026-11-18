// SPDX-License-Identifier: Apache-2.0
//
// Whether the on-screen keyboard is covering the app. Android resizes the
// web view, so the signal is "a text field has focus and the viewport lost a
// keyboard's worth of height compared with the tallest seen at this width".
//
// The same tallest height, measured in rem, says whether the screen is short
// for its text size (large text, a phone on its side). Then the pinned chrome
// packs itself down and confirm-style footers scroll with the content, so the
// content keeps a readable share of the screen. The keyboard never changes it.
// The visible height in rem, keyboard included, says whether it is cramped:
// then only the urgent controls and an open sheet's own action stay pinned,
// and everything else scrolls.
// The width in rem says whether it is narrow for its text size: then screen
// bars give the title its own row and the attention entry packs into a grid.
// Short and at least 600px wide, it is a phone on its side: then a screen may
// put its bar and its input side by side. A 360px phone turned, with Android's
// buttons at one side, is about 690px wide.

import { useSyncExternalStore } from "react";
import type { PaneScope } from "./pane";

let open = false;
let tight = false;
let narrow = false;
let cramped = false;
let sideways = false;
/** Below this many rem of height, pinned chrome would crowd out the content. */
const TIGHT_REM = 36;
/** Below this many rem of visible height, a second pinned row would leave the content a sliver. */
const CRAMPED_REM = 18;
/** Below this many rem of width, a bar's title and actions no longer share a row. */
const NARROW_REM = 22;
/** From this width a short screen lays its pinned rows side by side. */
const SIDEWAYS_PX = 600;
const tallest = new Map<number, number>();
const listeners = new Set<() => void>();
let installed = false;
let remSize = 16;
let revision = 0;
const paneTallest = new WeakMap<HTMLElement, Map<string, number>>();

interface LayoutState {
  tight: boolean;
  narrow: boolean;
  cramped: boolean;
  sideways: boolean;
}

/** Shared thresholds for a pane and the legacy whole-window layout. */
export function classifyLayout(width: number, height: number, maximumHeight: number, rem: number): LayoutState {
  const tight = maximumHeight / rem < TIGHT_REM;
  return {
    tight,
    narrow: width / rem < NARROW_REM,
    cramped: height / rem < CRAMPED_REM,
    sideways: tight && width >= SIDEWAYS_PX,
  };
}

function editable(el: Element | null): boolean {
  if (!el) return false;
  if (el instanceof HTMLTextAreaElement) return !el.readOnly;
  if (el instanceof HTMLInputElement) return !el.readOnly && !["button", "checkbox", "radio", "range", "submit", "file"].includes(el.type);
  return (el as HTMLElement).isContentEditable === true;
}

function measure() {
  const vv = window.visualViewport;
  const h = vv ? vv.height : window.innerHeight;
  const w = Math.round(window.innerWidth);
  const max = Math.max(tallest.get(w) ?? 0, h);
  tallest.set(w, max);
  const next = editable(document.activeElement) && max - h > 140;
  const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
  const short = max / rem < TIGHT_REM;
  const thin = w / rem < NARROW_REM;
  const low = h / rem < CRAMPED_REM;
  const turned = short && w >= SIDEWAYS_PX;
  if (next !== open || short !== tight || thin !== narrow || low !== cramped || turned !== sideways || rem !== remSize) {
    remSize = rem;
    revision++;
    open = next;
    tight = short;
    narrow = thin;
    cramped = low;
    sideways = turned;
    document.documentElement.classList.toggle("kb-open", open);
    document.documentElement.classList.toggle("app-tight", tight);
    document.documentElement.classList.toggle("app-narrow", narrow);
    document.documentElement.classList.toggle("app-cramped", cramped);
    document.documentElement.classList.toggle("app-sideways", sideways);
    for (const l of listeners) l();
  }
}

function install() {
  if (installed) return;
  installed = true;
  window.visualViewport?.addEventListener("resize", measure);
  window.addEventListener("resize", measure);
  // A test's page can be taken down before this runs.
  const later = () => setTimeout(() => typeof window !== "undefined" && measure(), 50);
  document.addEventListener("focusin", later);
  document.addEventListener("focusout", later);
  // A 1rem probe resizes when the text size changes without a window resize.
  if (typeof ResizeObserver !== "undefined") {
    const probe = document.createElement("div");
    probe.setAttribute("aria-hidden", "true");
    probe.style.cssText = "position:fixed;width:1rem;height:1rem;visibility:hidden;pointer-events:none;left:0;top:0";
    document.body.append(probe);
    new ResizeObserver(measure).observe(probe);
  }
  measure();
}

const subscribe = (l: () => void) => {
  install();
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
};

export function useKeyboardOpen(): boolean {
  return useSyncExternalStore(subscribe, () => open);
}

/** A pane shares the one viewport/text-size subscription, not another observer. */
export function usePaneLayout(pane: PaneScope | null): LayoutState {
  useSyncExternalStore(subscribe, () => revision);
  if (!pane?.root || pane.width <= 0 || pane.height <= 0) return { tight, narrow, cramped, sideways };
  let heights = paneTallest.get(pane.root);
  if (!heights) {
    heights = new Map();
    paneTallest.set(pane.root, heights);
  }
  // Entering/leaving split is a new layout, not a keyboard height change.
  const key = `${Math.round(pane.width)}:${pane.split}`;
  const maximumHeight = Math.max(heights.get(key) ?? 0, pane.height);
  heights.set(key, maximumHeight);
  return classifyLayout(pane.width, pane.height, maximumHeight, remSize);
}

