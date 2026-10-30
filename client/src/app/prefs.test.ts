// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";

afterEach(() => { vi.unstubAllGlobals(); localStorage.removeItem("prefs"); });

it.each([
  [false, false, false], [true, false, true],
  [false, true, true], [true, true, true],
])("app=%s and system=%s motion preferences agree for CSS and scrolling", async (app, system, expected) => {
  vi.resetModules();
  localStorage.removeItem("prefs");
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: query.includes("reduced-motion") && system,
    addEventListener: vi.fn(),
  }));
  const { prefs, motionReduced, apply } = await import("./prefs");
  prefs.set(p => ({ ...p, reduceMotion: app }));
  apply();
  expect(motionReduced()).toBe(expected);
  expect(document.documentElement.dataset.reduceMotion).toBe(String(expected));
});

it("tells a watcher when either motion preference changes, until it stops", async () => {
  vi.resetModules();
  localStorage.removeItem("prefs");
  const system = new EventTarget();
  vi.stubGlobal("matchMedia", (query: string) => Object.assign(system, { matches: false, media: query }));
  const { prefs, watchMotion } = await import("./prefs");
  const seen = vi.fn();
  const stop = watchMotion(seen);
  prefs.set(p => ({ ...p, reduceMotion: true }));
  system.dispatchEvent(new Event("change"));
  expect(seen).toHaveBeenCalledTimes(2);
  stop();
  prefs.set(p => ({ ...p, reduceMotion: false }));
  system.dispatchEvent(new Event("change"));
  expect(seen).toHaveBeenCalledTimes(2);
});

it("keeps only text screen names from saved preferences", async () => {
  vi.resetModules();
  localStorage.setItem("prefs", JSON.stringify({ screenNames: { "home DP-1": "Desk", "home HDMI-A-1": 3 } }));
  const { prefs } = await import("./prefs");
  expect(prefs.get().screenNames).toEqual({ "home DP-1": "Desk" });
});
