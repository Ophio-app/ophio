// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import type { Platform } from "../host/link";
import type { WindowLayout } from "../platform/windowLayout";
import { browserWindowLayout, splitGeometry } from "./split";

export function useSplitLayout(platform: Platform) {
  const [size, setSize] = useState(() => ({ width: window.innerWidth, height: window.innerHeight }));
  const [native, setNative] = useState<WindowLayout | null>(null);
  const [browser, setBrowser] = useState<WindowLayout | null>(browserWindowLayout);
  useEffect(() => {
    let baseline = { width: window.innerWidth, height: window.visualViewport?.height ?? window.innerHeight };
    const measure = () => {
      const width = window.innerWidth, actual = window.visualViewport?.height ?? window.innerHeight;
      const editing = document.activeElement?.matches("input, textarea, [contenteditable='true']") === true;
      // Keyboard animations arrive as many small resizes. Keep eligibility at
      // the pre-edit height throughout, including near the minimum split height.
      // PaneFrame still measures its actual, smaller visible space separately.
      // Width changes or an unfocused resize start a fresh window baseline.
      baseline = !editing || baseline.width !== width
        ? { width, height: actual }
        : { width, height: Math.max(baseline.height, actual) };
      const height = baseline.height;
      setSize(old => old.width === width && old.height === height ? old : { width, height });
      setBrowser(browserWindowLayout());
    };
    measure();
    window.addEventListener("resize", measure);
    window.visualViewport?.addEventListener("resize", measure);
    return () => { window.removeEventListener("resize", measure); window.visualViewport?.removeEventListener("resize", measure); };
  }, []);
  useEffect(() => {
    let current = true;
    let receivedEvent = false;
    const off = platform.onWindowLayout?.(layout => {
      receivedEvent = true;
      if (current) setNative(layout);
    });
    // The initial getter can finish after a newer event; never replace that
    // live geometry with its older snapshot.
    void platform.windowLayout?.().then(layout => { if (current && !receivedEvent) setNative(layout); }).catch(() => {});
    return () => { current = false; off?.(); };
  }, [platform]);
  // A stale geometry snapshot after rotation must not put a crease in the
  // wrong place. The next native update will reinstate precise placement.
  const currentNative = native && Math.abs(native.width - size.width) < 2 ? native : null;
  const layout = currentNative?.folds.length ? currentNative : browser;
  return { ...size, layout, geometry: splitGeometry(size.width, size.height, layout) };
}
