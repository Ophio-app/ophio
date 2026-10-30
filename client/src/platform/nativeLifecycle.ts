// SPDX-License-Identifier: Apache-2.0
import { invoke } from "@tauri-apps/api/core";
import type { AppLockState, Platform } from "../host/link";

export type LifecycleEvent =
  | { type: "appLink"; url: string }
  | { type: "lockChanged"; state: AppLockState };

/** Buffered cold-start VIEW intents survive asynchronous profile loading. */
export function nativeLifecycle() {
  const links = new Set<(url: string) => void>();
  const locks = new Set<(state: AppLockState) => void>();
  const waiting: string[] = [];
  const call = async (command: string, args?: Record<string, unknown>): Promise<AppLockState> => {
    try { return await invoke<AppLockState>(command, args); }
    catch (e) {
      throw new Error(e && typeof e === "object" && "message" in e && typeof e.message === "string"
        ? e.message : "This device couldn’t verify its screen lock. Try again.");
    }
  };
  const capabilities: Pick<Platform, "appLock" | "onAppLink"> = {
    appLock: {
      state: () => call("app_lock_state"),
      configure: (enabled, relockMinutes) => call("app_lock_configure", { enabled, relockMinutes }),
      unlock: () => call("app_unlock"),
      confirm: (title) => call("app_confirm", { title }),
      lock: () => call("app_lock"),
      listen(listener) { locks.add(listener); return () => { locks.delete(listener); }; },
    },
    onAppLink(listener) {
      links.add(listener);
      queueMicrotask(() => {
        if (links.has(listener)) for (const url of waiting.splice(0)) listener(url);
      });
      return () => { links.delete(listener); };
    },
  };
  return {
    capabilities,
    receive(event: LifecycleEvent) {
      if (event.type === "lockChanged") for (const listener of locks) listener(event.state);
      else if (links.size) for (const listener of links) listener(event.url);
      else { waiting.push(event.url); if (waiting.length > 8) waiting.shift(); }
    },
  };
}
