// SPDX-License-Identifier: Apache-2.0
import { useEffect } from "react";
import type { Platform } from "../host/link";
import { reportPlatformError } from "./errors";

const owners = new WeakMap<Platform, Set<symbol>>();
/** Reference counted: a media view and a human lease may overlap. Background
 * releases synchronously; only a still-mounted foreground owner can reacquire. */
export function holdAwake(platform: Platform): () => void {
  if (!platform.keepAwake) return () => {};
  let set = owners.get(platform);
  if (!set) {
    set = new Set();
    owners.set(platform, set);
  }
  const token = Symbol();
  const call = (on: boolean) => {
    try {
      platform.keepAwake?.(on);
    } catch {
      reportPlatformError({
        operation: "Keep screen awake",
        message: "This device couldn’t change its screen timeout.",
      });
    }
  };
  const release = () => {
    if (set.delete(token) && set.size === 0) call(false);
  };
  const changed = () => {
    if (document.visibilityState !== "visible") {
      release();
      return;
    }
    if (!set.has(token)) {
      set.add(token);
      if (set.size === 1) call(true);
    }
  };
  document.addEventListener("visibilitychange", changed);
  window.addEventListener("pagehide", release);
  window.addEventListener("pageshow", changed);
  changed();
  return () => {
    document.removeEventListener("visibilitychange", changed);
    window.removeEventListener("pagehide", release);
    window.removeEventListener("pageshow", changed);
    release();
  };
}
export function useKeepAwake(platform: Platform, active: boolean): void {
  useEffect(
    () => (active ? holdAwake(platform) : undefined),
    [platform, active],
  );
}
