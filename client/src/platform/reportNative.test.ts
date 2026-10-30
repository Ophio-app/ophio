// SPDX-License-Identifier: Apache-2.0
import { beforeEach, expect, it, vi } from "vitest";
import type { CrashRecord } from "../protocol/generated/CrashRecord";

const bridge = vi.hoisted(() => ({ invoke: vi.fn(), isTauri: vi.fn(), channels: [] as { onmessage: (value: unknown) => void }[] }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: bridge.invoke,
  isTauri: bridge.isTauri,
  Channel: class {
    onmessage = (_value: unknown) => {};
    constructor() { bridge.channels.push(this); }
  },
}));
const tick = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };
beforeEach(() => {
  vi.resetModules();
  bridge.invoke.mockReset();
  bridge.isTauri.mockReturnValue(true);
  bridge.channels.length = 0;
});

it("is inert in a browser or simulator without touching the bridge", async () => {
  bridge.isTauri.mockReturnValue(false);
  const native = await import("./reportNative");
  expect(await native.takeNativeCrashes()).toEqual({ crashes: [] });
  expect(await native.captureOwnView()).toBeNull();
  expect(await native.screenshotSupported()).toBe(false);
  native.onScreenshot(vi.fn())();
  await tick();
  expect(bridge.invoke).not.toHaveBeenCalled();
});

it("accepts desktop capability fallbacks and native failures", async () => {
  const native = await import("./reportNative");
  bridge.invoke.mockImplementation(async (command: string) => command === "take_native_crashes" ? { crashes: [] } : command === "capture_own_view" ? null : false);
  expect(await native.takeNativeCrashes()).toEqual({ crashes: [] });
  expect(await native.captureOwnView()).toBeNull();
  expect(await native.screenshotSupported()).toBe(false);
  native.onScreenshot(vi.fn())();
  await tick();
  expect(bridge.channels).toHaveLength(0);
  bridge.invoke.mockRejectedValue(new Error("unavailable"));
  expect(await native.takeNativeCrashes()).toEqual({ crashes: [] });
  expect(await native.captureOwnView()).toBeNull();
  expect(await native.screenshotSupported()).toBe(false);
});

it("combines native sources newest first, says when the app was updated, and drains them once", async () => {
  const native = await import("./reportNative");
  const crash = (at: number, kind: CrashRecord["kind"]): CrashRecord => ({ at, kind, side: "phone", code: kind === "panic" ? "panic:client/src-tauri/src/lib.rs:1" : "exit:anr", build: "test", frames: [] });
  bridge.invoke.mockResolvedValueOnce({ crashes: [crash(10, "panic"), crash(30, "system")], updatedAt: 5 }).mockResolvedValueOnce({ crashes: [] });
  const taken = await native.takeNativeCrashes();
  expect(taken.crashes.map(c => c.at)).toEqual([30, 10]);
  expect(taken.updatedAt).toBe(5);
  expect(await native.takeNativeCrashes()).toEqual({ crashes: [] });
  expect(bridge.invoke).toHaveBeenCalledWith("take_native_crashes");
});

it("shares one native watch, dispatches screenshots, and releases the last owner", async () => {
  const native = await import("./reportNative");
  bridge.invoke.mockResolvedValue(true);
  const first = vi.fn(), second = vi.fn();
  const stopFirst = native.onScreenshot(first), stopSecond = native.onScreenshot(second);
  await tick();
  expect(bridge.invoke.mock.calls.filter(([name]) => name === "start_screenshot_watch")).toHaveLength(1);
  bridge.channels[0]!.onmessage({ type: "screenshot" });
  expect(first).toHaveBeenCalledTimes(1);
  expect(second).toHaveBeenCalledTimes(1);
  stopFirst(); await tick();
  bridge.channels[0]!.onmessage({ type: "screenshot" });
  expect(first).toHaveBeenCalledTimes(1);
  expect(second).toHaveBeenCalledTimes(2);
  stopSecond(); stopSecond(); await tick();
  expect(bridge.invoke.mock.calls.filter(([name]) => name === "stop_screenshot_watch")).toHaveLength(1);
});

it("stops a watch whose asynchronous start finishes after unsubscribe", async () => {
  const native = await import("./reportNative");
  let finish = (_value: boolean) => {};
  bridge.invoke.mockImplementation((command: string) => command === "start_screenshot_watch" ? new Promise<boolean>(resolve => { finish = resolve; }) : Promise.resolve(true));
  const listener = vi.fn();
  const stop = native.onScreenshot(listener);
  await tick(); stop(); finish(true); await tick();
  expect(bridge.invoke).toHaveBeenCalledWith("stop_screenshot_watch");
  bridge.channels[0]!.onmessage({ type: "screenshot" });
  expect(listener).not.toHaveBeenCalled();
});

it("returns an own-view picture without requiring screenshot detection support", async () => {
  const native = await import("./reportNative");
  const picture = { media_type: "image/jpeg", data: "AQ==", width: 10, height: 20 };
  bridge.invoke.mockResolvedValueOnce(picture);
  expect(await native.captureOwnView()).toEqual(picture);
  expect(bridge.invoke).toHaveBeenCalledWith("capture_own_view");
});
