// SPDX-License-Identifier: Apache-2.0
import { beforeEach, expect, it, vi } from "vitest";
import { createNativePlatform } from "./native";
import { unavailableWindowLayout, type WindowLayout } from "../platform/windowLayout";
const native = vi.hoisted(() => ({ invoke: vi.fn(), events: undefined as undefined | { onmessage(value: unknown): void } }));
vi.mock("@tauri-apps/api/core", () => ({
  Channel: class { onmessage = (_value: unknown) => {}; }, invoke: native.invoke,
}));
const tick = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };
const folded: WindowLayout = { availability: "available", width: 800, height: 900,
  folds: [{ bounds: { x: 400, y: 0, width: 0, height: 900 }, orientation: "vertical",
    state: "flat", separating: false, occlusion: "none" }] };
beforeEach(() => {
  native.events = undefined;
  native.invoke.mockReset();
  native.invoke.mockImplementation(async (command: string, args?: { events: typeof native.events }) => {
    if (command === "device_events") native.events = args?.events;
    if (command === "window_layout") return folded;
    return undefined;
  });
});
it("reads the native command and replays the latest snapshot for late subscribers", async () => {
  const platform = createNativePlatform();
  expect(await platform.windowLayout?.()).toEqual(folded);
  const listener = vi.fn();
  const off = platform.onWindowLayout?.(listener);
  await tick();
  expect(listener).toHaveBeenCalledExactlyOnceWith(folded);
  off?.();
  native.events?.onmessage({ type: "windowLayoutChanged", layout: unavailableWindowLayout(390, 844) });
  expect(listener).toHaveBeenCalledTimes(1);
});
it("delivers changed geometry once without replaying an older queued snapshot", async () => {
  const platform = createNativePlatform();
  await platform.windowLayout?.();
  const listener = vi.fn();
  platform.onWindowLayout?.(listener);
  const unfolded: WindowLayout = { ...folded, folds: [] };
  native.events?.onmessage({ type: "windowLayoutChanged", layout: unfolded });
  await tick();
  expect(listener).toHaveBeenCalledExactlyOnceWith(unfolded);
});
it("does not overwrite a newer event with a command that was already running", async () => {
  const platform = createNativePlatform();
  let finish!: (layout: WindowLayout) => void;
  native.invoke.mockImplementation((command: string) => command === "window_layout"
    ? new Promise<WindowLayout>(resolve => { finish = resolve; }) : Promise.resolve());
  const reading = platform.windowLayout?.();
  const changed = unavailableWindowLayout(390, 844);
  native.events?.onmessage({ type: "windowLayoutChanged", layout: changed });
  finish(folded);
  expect(await reading).toEqual(changed);
});
it("clears bad native geometry rather than preserving a stale crease", async () => {
  const platform = createNativePlatform();
  await platform.windowLayout?.();
  const listener = vi.fn();
  platform.onWindowLayout?.(listener);
  native.events?.onmessage({ type: "windowLayoutChanged", layout: { ...folded, width: Infinity } });
  expect(listener).toHaveBeenLastCalledWith(unavailableWindowLayout());
});
