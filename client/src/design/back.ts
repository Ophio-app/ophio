// SPDX-License-Identifier: Apache-2.0
//
// System back (Android back gesture, browser back) as one ordered stack.
// Open layers — sheets, full-screen editors — close first, then the current
// destination's own stack. A single guard history entry exists only while
// there is something in-app to go back to, so back at the root leaves the app.
// Android asks `systemBack` directly instead: its WebView skips history
// entries a page added without a tap, such as a guard re-armed after Back,
// and would then leave the app from a screen that has somewhere to go back to.

import { useEffect, useRef } from "react";
import { flushSync } from "react-dom";

interface Layer {
  onBack: () => void;
}

interface NavBack {
  canGoBack: () => boolean;
  back: () => void;
}

const layers: Layer[] = [];
let nav: NavBack | null = null;
let armed = false;
let ignorePops = 0;
let installed = false;

function install(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  window.addEventListener("popstate", () => {
    if (ignorePops > 0) {
      ignorePops--;
      sync();
      return;
    }
    armed = false;
    goBack();
    sync();
  });
  window.systemBack = () => {
    const handled = goBack();
    sync();
    return handled;
  };
}

/** One step back in the app; false when there is nowhere left to go. */
function goBack(): boolean {
  // Keep the layer registered until its owner accepts closing it.
  const top = layers.at(-1);
  if (top) flushSync(top.onBack);
  else if (nav?.canGoBack()) nav.back();
  else return false;
  return true;
}

declare global {
  interface Window {
    /** Called by the Android shell on system Back; false means leave the app. */
    systemBack?: () => boolean;
  }
}

function wanted(): boolean {
  return layers.length > 0 || (nav?.canGoBack() ?? false);
}

/** Arm or disarm the guard entry to match the current in-app back state. */
export function sync(): void {
  install();
  // history.back() is asynchronous. Never rearm or disarm a second guard
  // until that traversal completes (for example, sheet → destination).
  if (ignorePops > 0) return;
  if (wanted() && !armed) {
    history.pushState({ guard: true }, "");
    armed = true;
  } else if (!wanted() && armed) {
    armed = false;
    ignorePops++;
    history.back();
  }
}

export function setNavBack(n: NavBack | null): void {
  nav = n;
  sync();
}

/** Register a layer that system back asks to close while `open` is true. */
export function useBackLayer(open: boolean, onBack: () => void): void {
  const cb = useRef(onBack);
  cb.current = onBack;
  useEffect(() => {
    if (!open) return;
    const layer: Layer = { onBack: () => cb.current() };
    layers.push(layer);
    sync();
    return () => {
      const i = layers.indexOf(layer);
      if (i >= 0) layers.splice(i, 1);
      sync();
    };
  }, [open]);
}
