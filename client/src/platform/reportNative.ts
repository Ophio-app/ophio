// SPDX-License-Identifier: Apache-2.0
import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import type { CrashRecord } from "../protocol/generated/CrashRecord";
import type { ReportScreenshot } from "../protocol/generated/ReportScreenshot";

export type NativeReportScreenshot = Pick<ReportScreenshot, "media_type" | "data" | "width" | "height">;
const native = () => import.meta.env.MODE !== "sim" && isTauri();

/** The phone's crashes, and when this app was installed or last updated. */
export type NativeCrashes = { crashes: CrashRecord[]; updatedAt?: number };
export async function takeNativeCrashes(): Promise<NativeCrashes> {
  if (!native()) return { crashes: [] };
  try {
    const taken = await invoke<NativeCrashes>("take_native_crashes");
    return { ...taken, crashes: taken.crashes.sort((a, b) => b.at - a.at) };
  } catch { return { crashes: [] }; }
}

export async function screenshotSupported(): Promise<boolean> {
  if (import.meta.env.MODE === "sim") return (await import("../dev/sim/reportPhone")).reportPhone.detection;
  if (!native()) return false;
  try { return await invoke<boolean>("screenshot_supported"); }
  catch { return false; }
}

const listeners = new Set<() => void>();
let watching = false;
let transition = Promise.resolve();

function updateWatch(): void {
  // Serialize start/stop so a slow start cannot outlive the last unsubscribe.
  transition = transition.catch(() => {}).then(async () => {
    if (listeners.size && !watching) {
      if (!await screenshotSupported() || !listeners.size) return;
      const events = new Channel<{ type: "screenshot" }>();
      events.onmessage = (event) => {
        if (event.type === "screenshot") for (const listener of [...listeners]) listener();
      };
      watching = await invoke<boolean>("start_screenshot_watch", { events });
    } else if (!listeners.size && watching) {
      watching = false;
      await invoke("stop_screenshot_watch");
    }
  }).catch(() => {});
}

export function onScreenshot(cb: () => void): () => void {
  if (import.meta.env.MODE === "sim") {
    let stopped = false;
    void import("../dev/sim/reportPhone").then(({ reportPhone }) => { if (!stopped) reportPhone.listeners.add(cb); });
    return () => { stopped = true; void import("../dev/sim/reportPhone").then(({ reportPhone }) => reportPhone.listeners.delete(cb)); };
  }
  if (!native()) return () => {};
  // Each subscription owns its place even when the callback is shared.
  const listener = () => cb();
  listeners.add(listener);
  updateWatch();
  return () => {
    if (listeners.delete(listener)) updateWatch();
  };
}

export async function captureOwnView(): Promise<NativeReportScreenshot | null> {
  if (import.meta.env.MODE === "sim") return (await import("../dev/sim/reportPhone")).captureReportView();
  if (!native()) return null;
  try { return await invoke<NativeReportScreenshot | null>("capture_own_view"); }
  catch { return null; }
}
