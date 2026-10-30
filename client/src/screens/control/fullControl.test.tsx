// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { prefs } from "../../app/prefs";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { Shell } from "../../shell/Shell";
import { clampPan, followPointer, twoFingerIntent, TWO_FINGER_SLOP } from "./gestures";
import { screenGravity, tiltVelocity, TILT_SPEED } from "./tilt";
import type { Picture } from "./useMedia";

// A display full screen: on its side, or while this phone controls it, the
// picture fills the screen and the controls lie over it, hide in one tap, and
// keep every running task's Stop at most two taps away (D-054).
let app: App;
let landscape = false;
// A current picture, so the computer can be taken over.
const picture = { current: { at: 0, revision: 1, connected: true, error: null } as Picture };
vi.mock("./useMedia", async (importOriginal) => ({
  ...await importOriginal<typeof import("./useMedia")>(),
  useMedia: () => ({ picture: picture.current, latest: picture, canvas: { current: null }, retry: vi.fn(), reopen: vi.fn() }),
}));
const immersive = vi.fn();
const flush = () => act(async () => { await vi.advanceTimersByTimeAsync(900); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers(); landscape = false; immersive.mockClear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  HTMLElement.prototype.setPointerCapture ??= () => {};
  HTMLElement.prototype.releasePointerCapture ??= () => {};
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: query.includes("landscape") ? landscape : false, media: query,
    addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {},
  }));
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = { ...createSimPlatform(), immersive };
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
  app.ui.selectProject(ids.pTide);
});
afterEach(() => {
  cleanup(); prefs.set((p) => ({ ...p, tiltPan: false }));
  app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
});

async function open(targetId = ids.tgHdmi) {
  render(<AppProvider app={app}><Shell /></AppProvider>);
  await flush();
  act(() => app.ui.selectTab("control"));
  picture.current = { ...picture.current, at: Date.now(), revision: app.session.state.get().targets.find((t) => t.target_id === targetId)?.geometry.revision ?? 1 };
  act(() => app.ui.push({ screen: "desktop", targetId }));
  await vi.waitFor(async () => {
    await act(async () => { await vi.dynamicImportSettled(); });
    expect(document.querySelector(".desktop-screen")).not.toBeNull();
  }, { timeout: 10_000 });
  await flush();
}
const full = () => document.querySelector(".app")!.classList.contains("app--full");
const shown = () => within([...document.querySelectorAll<HTMLElement>(".route")].find((r) => !r.classList.contains("is-hidden") && r.closest(".tab:not(.is-hidden)"))!);

it("upright and only watching, a display keeps the shell", async () => {
  await open();
  expect(full()).toBe(false);
  expect(document.querySelector("nav.nav")).not.toBeNull();
  expect(document.querySelector(".full-overlay")).toBeNull();
  expect(immersive).not.toHaveBeenCalledWith(true);
});

it("on its side a display fills the screen and hides the system bars until it is left", async () => {
  landscape = true;
  await open();
  expect(full()).toBe(true);
  expect(document.querySelector("nav.nav")).toBeNull();
  expect(immersive).toHaveBeenLastCalledWith(true);
  const bars = shown();
  expect(bars.getByRole("heading", { name: "Screen 2 (HDMI-A-1)" })).toBeTruthy();
  expect(bars.getByRole("status").textContent).toMatch(/Watching/);
  expect(bars.getByRole("button", { name: "Take over" })).toBeTruthy();
  // Not controlling, so no pointer tools yet.
  expect(bars.queryByRole("button", { name: "Right click" })).toBeNull();
  fireEvent.click(bars.getByRole("button", { name: "Back" }));
  await flush();
  expect(full()).toBe(false);
  expect(immersive).toHaveBeenLastCalledWith(false);
});

it("taking control fills the screen upright too, with the pointer and Leave control over the picture", async () => {
  await open();
  fireEvent.click(shown().getAllByRole("button", { name: "Take over" })[0]!);
  await flush();
  expect(full()).toBe(true);
  const bars = shown();
  expect(bars.getByRole("status").textContent).toMatch(/You have control/);
  expect(bars.getByRole("button", { name: /Leave control/ })).toBeTruthy();
  expect(bars.getByRole("button", { name: "Keyboard" })).toBeTruthy();
  // Touch first: Right click is for the next click, and a finger drags.
  const right = bars.getByRole("button", { name: "Right click" });
  fireEvent.click(right);
  expect(right.getAttribute("aria-pressed")).toBe("true");
  expect(bars.queryByRole("button", { name: "Drag" })).toBeNull();
  fireEvent.click(bars.getByRole("button", { name: "Pointer: touch. Switch" }));
  const drag = bars.getByRole("button", { name: "Drag" });
  fireEvent.click(drag);
  expect(drag.getAttribute("aria-pressed")).toBe("true");
  expect(localStorage.getItem("control.pointer-mode")).toBe("touchpad");
});

it("hides in one tap, leaving Show controls and the running tasks' Stops two taps away", async () => {
  landscape = true;
  await open();
  const view = shown();
  fireEvent.click(view.getByRole("button", { name: "Hide controls" }));
  expect(view.queryByRole("button", { name: "Zoom in" })).toBeNull();
  expect(view.queryByRole("heading", { name: "Screen 2 (HDMI-A-1)" })).toBeNull();
  const handle = within(view.getByRole("group", { name: "Controls hidden" }));
  fireEvent.click(handle.getByRole("button", { name: /running tasks?\. Show Stop$/ }));
  await flush();
  const rail = within(document.querySelector<HTMLElement>(".safety-rail--expanded")!);
  expect(rail.getAllByRole("button", { name: /^Stop · / }).length).toBeGreaterThan(0);
  fireEvent.click(handle.getByRole("button", { name: "Show controls" }));
  expect(view.getByRole("button", { name: "Zoom in" })).toBeTruthy();
});

it("keeps the Stop of the task driving the display on the overlay itself", async () => {
  landscape = true;
  await open(ids.tgDp1);
  const dash = app.session.state.get().tasks[ids.tDash]!;
  expect(shown().getByRole("button", { name: new RegExp(`^Stop · ${dash.title}`) })).toBeTruthy();
});

it("offers Jev in place, and says when the computer can't let Jev control its desktop", async () => {
  landscape = true;
  await open();
  fireEvent.click(shown().getByRole("button", { name: "Let Jev control Screen 2 (HDMI-A-1)" }));
  await flush();
  // The display is already chosen; there's nothing to pick.
  let sheet = screen.getByRole("dialog", { name: "Start voice control" });
  expect(within(sheet).queryByRole("combobox")).toBeNull();
  expect((within(sheet).getByRole("button", { name: "Accept and preview commands" }) as HTMLButtonElement).disabled).toBe(true);
  cleanup();
  const host = window.__sim!.host;
  host.voice.link(host); host.voice.desktopAvailable = true;
  await open();
  fireEvent.click(shown().getByRole("button", { name: "Let Jev control Screen 2 (HDMI-A-1)" }));
  await flush();
  sheet = screen.getByRole("dialog", { name: "Start voice control" });
  expect((within(sheet).getByRole("button", { name: "Accept and preview commands" }) as HTMLButtonElement).disabled).toBe(false);
});

it("Tilt to pan is remembered", async () => {
  landscape = true;
  await open();
  const tilt = shown().getByRole("button", { name: "Tilt to pan" });
  expect(tilt.getAttribute("aria-pressed")).toBe("false");
  fireEvent.click(tilt);
  expect(prefs.get().tiltPan).toBe(true);
  expect(shown().getByRole("button", { name: "Tilt to pan" }).getAttribute("aria-pressed")).toBe("true");
});

describe("gestures", () => {
  const size = { width: 400, height: 200 };
  it("keeps a zoomed picture covering the screen", () => {
    expect(clampPan({ zoom: 1, pan: { x: 30, y: -5 } }, size)).toEqual({ zoom: 1, pan: { x: 0, y: 0 } });
    expect(clampPan({ zoom: 2, pan: { x: 30, y: -500 } }, size)).toEqual({ zoom: 2, pan: { x: 0, y: -200 } });
  });
  it("tells a pinch from a two-finger scroll once either passes the slop", () => {
    const at = { x: 100, y: 100 };
    expect(twoFingerIntent(100, 100 + TWO_FINGER_SLOP - 1, at, { x: 105, y: 100 })).toBeNull();
    expect(twoFingerIntent(100, 160, at, { x: 105, y: 100 })).toBe("zoom");
    expect(twoFingerIntent(100, 104, at, { x: 100, y: 140 })).toBe("scroll");
  });
  it("moves a zoomed view to keep the pointer in sight", () => {
    const view = { zoom: 2, pan: { x: 0, y: 0 } };
    // The pointer at the right of the desktop is off the right of the screen.
    const moved = followPointer(view, { x: 0.9, y: 0.25 }, size);
    expect(moved.pan.x).toBeLessThan(0);
    expect(400 * 0.9 * 2 + moved.pan.x).toBeLessThanOrEqual(400 * 0.85 + 0.001);
    expect(moved.pan.y).toBe(0);
    expect(followPointer({ zoom: 1, pan: { x: 0, y: 0 } }, { x: 0.99, y: 0.99 }, size).pan).toEqual({ x: 0, y: 0 });
  });
});

describe("tilt", () => {
  const level = { x: 0, y: 0 };
  const tipped = (deg: number) => Math.sin((deg * Math.PI) / 180);
  it("turns the phone's axes into the screen's as the screen rotates", () => {
    // Held on its left side (screen rotated 90°): gravity along the phone's
    // x axis is along the screen's y axis.
    const g = screenGravity({ x: 9.81, y: 0, z: 0 }, 90)!;
    expect(g.x).toBeCloseTo(0);
    expect(g.y).toBeCloseTo(1);
    expect(screenGravity({ x: 0, y: 0, z: 0 }, 0)).toBeNull();
  });
  it("ignores a hand's tremor and speeds up the further it tips", () => {
    expect(tiltVelocity(level, { x: tipped(3), y: tipped(-3) })).toEqual({ x: 0, y: 0 });
    const some = tiltVelocity(level, { x: tipped(12), y: 0 }).x;
    const more = tiltVelocity(level, { x: tipped(20), y: 0 }).x;
    expect(some).toBeGreaterThan(0);
    expect(more).toBeGreaterThan(some);
    expect(tiltVelocity(level, { x: tipped(40), y: 0 }).x).toBeCloseTo(TILT_SPEED);
    expect(tiltVelocity(level, { x: 0, y: tipped(40) }).y).toBeCloseTo(-TILT_SPEED);
  });
});
