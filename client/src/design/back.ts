// SPDX-License-Identifier: Apache-2.0
//
// System back (Android back gesture, browser back) as one ordered stack.
// Open layers — sheets, full-screen editors — close first, then the current
// destination's own stack. A single guard history entry exists only while
// there is something in-app to go back to, so back at the root leaves the app.
// Android asks `systemBack` directly instead: its WebView skips history
// entries a page added without a tap, such as a guard re-armed after Back,
// and would then leave the app from a screen that has somewhere to go back to.

import { useContext, useEffect, useRef } from "react";
import { flushSync } from "react-dom";
import { usePane } from "../shell/pane";
import { SurfaceActive } from "./surface";

interface Layer {
  owner: string | null;
  nativeModal: boolean;
  /** When the layer opened, if it can re-register out of order. */
  order?: number;
  eligible: () => boolean;
  onBack: () => void;
}

export interface NavBack {
  canGoBack: () => boolean;
  back: () => void;
}

const layers: Layer[] = [];
const navs = new Map<string | null, NavBack>();
let activePane: string | null = null;

function topLayer(): Layer | undefined {
  // Native dialogs cover the whole document in showModal order, including
  // single-view pane dialogs. App-wide layers still precede contained sheets.
  return layers.findLast((layer) => layer.nativeModal && layer.eligible()) ??
    layers.findLast((layer) => layer.owner === null && layer.eligible()) ??
    layers.findLast((layer) => layer.owner === activePane && layer.eligible());
}

function activeNav(): NavBack | undefined {
  return navs.get(activePane);
}
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
  const top = topLayer();
  const nav = activeNav();
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
  return !!topLayer() || (activeNav()?.canGoBack() ?? false);
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

export function setNavBack(n: NavBack | null, owner?: string): void {
  const key = owner ?? null;
  if (n) navs.set(key, n);
  else navs.delete(key);
  sync();
}

/** The outer split-view coordinator alone chooses where system Back goes. */
export function setActiveBackPane(id: string | null): void {
  activePane = id;
  sync();
}

/** Register a layer that system back asks to close while `open` is true.
 * `nativeModal` is only for a dialog opened with showModal(), not pane activity.
 * `order` places layers that re-register out of order, such as nested sheets
 * when the window folds, by when they were opened rather than registered. */
export function useBackLayer(open: boolean, onBack: () => void, { nativeModal = false, order }: { nativeModal?: boolean; order?: () => number } = {}): void {
  const pane = usePane();
  const surface = useContext(SurfaceActive);
  const current = useRef({ onBack, pane, surface, order });
  current.current = { onBack, pane, surface, order };
  const owner = pane?.id ?? null;
  useEffect(() => {
    if (!open) return;
    const layer: Layer = {
      owner,
      nativeModal,
      eligible: () => {
        const { pane, surface } = current.current;
        return surface && (!pane || (pane.visible && (nativeModal || pane.active)));
      },
      onBack: () => current.current.onBack(),
    };
    layer.order = current.current.order?.();
    const at = layer.order;
    const later = at === undefined ? -1 : layers.findIndex((other) => other.order !== undefined && other.order > at);
    layers.splice(later === -1 ? layers.length : later, 0, layer);
    sync();
    return () => {
      const i = layers.indexOf(layer);
      if (i >= 0) layers.splice(i, 1);
      sync();
    };
  }, [open, owner, nativeModal]);
  // Eligibility changes must also update the single browser history guard.
  useEffect(sync, [pane?.active, pane?.visible, surface]);
}
