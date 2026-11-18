// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Platform } from "../host/link";
import { unavailableWindowLayout, type WindowLayout } from "../platform/windowLayout";
import { useSplitLayout } from "./useSplitLayout";

class TestViewport extends EventTarget { height = 800; }
let viewport: TestViewport;
let field: HTMLTextAreaElement;
const platform = {} as Platform;

beforeEach(() => {
  viewport = new TestViewport();
  vi.stubGlobal("visualViewport", viewport);
  vi.stubGlobal("innerWidth", 1280);
  vi.stubGlobal("innerHeight", 800);
  field = document.createElement("textarea");
  document.body.append(field);
});
afterEach(() => { cleanup(); field.remove(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

function resize(height: number, width = window.innerWidth, windowHeight = window.innerHeight) {
  vi.stubGlobal("innerWidth", width);
  vi.stubGlobal("innerHeight", windowHeight);
  viewport.height = height;
  act(() => { viewport.dispatchEvent(new Event("resize")); window.dispatchEvent(new Event("resize")); });
}

it("keeps split eligible through small steps of an animated keyboard opening", () => {
  const { result } = renderHook(() => useSplitLayout(platform));
  act(() => field.focus());
  for (const height of [750, 700, 650, 600, 550, 500]) {
    resize(height);
    expect(result.current.geometry.eligible, `visible height ${height}`).toBe(true);
  }
  expect(result.current.height).toBe(800);
  // Eligibility is stable; the actual visible space has not been enlarged.
  expect(window.visualViewport?.height).toBe(500);
  expect(window.innerHeight).toBe(800);
});

it("keeps eligibility stable even before keyboard shrink reaches 140 pixels", () => {
  resize(600, 1280, 600);
  const { result } = renderHook(() => useSplitLayout(platform));
  act(() => field.focus());
  resize(550, 1280, 550);
  expect(result.current).toMatchObject({ height: 600, geometry: { eligible: true } });
});

it("also retains the baseline when Android resizes the layout viewport at every animation step", () => {
  const { result } = renderHook(() => useSplitLayout(platform));
  act(() => field.focus());
  for (const height of [750, 700, 650, 600, 550, 500]) resize(height, 1280, height);
  expect(result.current).toMatchObject({ width: 1280, height: 800, geometry: { eligible: true } });
  expect(window.innerHeight).toBe(500);
});

it("recovers the full visible height when the keyboard closes", () => {
  const { result } = renderHook(() => useSplitLayout(platform));
  act(() => field.focus());
  for (const height of [750, 700, 650, 600, 550, 500, 550, 600, 650, 700, 750, 800]) {
    resize(height);
    expect(result.current.geometry.eligible).toBe(true);
  }
  expect(result.current.height).toBe(800);
  act(() => field.blur());
  resize(800);
  expect(result.current.height).toBe(800);
});

it("refuses a genuinely short window after focus leaves the text field", () => {
  const { result } = renderHook(() => useSplitLayout(platform));
  act(() => field.focus());
  resize(500);
  expect(result.current.geometry.eligible).toBe(true);
  act(() => field.blur());
  resize(500, 1280, 500);
  expect(result.current).toMatchObject({ height: 500, geometry: { eligible: false } });
  // A new focus cannot resurrect an old, taller window baseline.
  act(() => field.focus());
  resize(450, 1280, 450);
  expect(result.current.geometry.eligible).toBe(false);
});

it("uses real unfocused resize steps rather than retaining the tallest window ever seen", () => {
  const { result } = renderHook(() => useSplitLayout(platform));
  for (const height of [750, 700, 650, 600, 550, 500]) {
    resize(height, 1280, height);
    expect(result.current.height).toBe(height);
    expect(result.current.geometry.eligible).toBe(height >= 560);
  }
});

it("starts a new baseline after width changes, including returning to an earlier width", () => {
  const { result } = renderHook(() => useSplitLayout(platform));
  act(() => field.focus());
  resize(500);
  expect(result.current.height).toBe(800);
  resize(412, 915, 412);
  expect(result.current).toMatchObject({ width: 915, height: 412, geometry: { eligible: false } });
  resize(500, 1280, 500);
  expect(result.current).toMatchObject({ width: 1280, height: 500, geometry: { eligible: false } });
  resize(800, 1280, 800);
  expect(result.current.geometry.eligible).toBe(true);
});

function foldedLayout(crease: number, height = 800): WindowLayout {
  return {
    availability: "available", width: 1280, height,
    folds: [{
      bounds: { x: crease, y: 0, width: 0, height }, orientation: "vertical",
      state: "half-opened", separating: true, occlusion: "none",
    }],
  };
}

it("does not let a late initial native snapshot overwrite a newer geometry event", async () => {
  let update: ((layout: WindowLayout) => void) | undefined;
  let resolve: ((layout: WindowLayout) => void) | undefined;
  const native: Platform = {
    ...platform,
    onWindowLayout: listener => { update = listener; return () => {}; },
    windowLayout: () => new Promise<WindowLayout>(done => { resolve = done; }),
  };
  const { result } = renderHook(() => useSplitLayout(native));
  const live = foldedLayout(500);
  act(() => update?.(live));
  expect(result.current.layout).toBe(live);
  await act(async () => { resolve?.(foldedLayout(640)); await Promise.resolve(); });
  expect(result.current.layout).toBe(live);
  // Native fold bounds can shrink with the keyboard while eligibility stays tall.
  act(() => field.focus());
  resize(500, 1280, 500);
  const keyboardLayout = foldedLayout(500, 500);
  act(() => update?.(keyboardLayout));
  expect(result.current.layout).toBe(keyboardLayout);
  expect(result.current).toMatchObject({ height: 800, geometry: { eligible: true } });
});

it("uses the initial native snapshot when no newer event has arrived", async () => {
  const snapshot = foldedLayout(500);
  const native: Platform = { ...platform, windowLayout: () => Promise.resolve(snapshot) };
  const { result } = renderHook(() => useSplitLayout(native));
  await act(async () => { await Promise.resolve(); });
  expect(result.current.layout).toBe(snapshot);
});

it("removes both resize listeners and the native subscription, and ignores late native snapshots", async () => {
  const addWindow = vi.spyOn(window, "addEventListener");
  const removeWindow = vi.spyOn(window, "removeEventListener");
  const addViewport = vi.spyOn(viewport, "addEventListener");
  const removeViewport = vi.spyOn(viewport, "removeEventListener");
  const off = vi.fn();
  let update: ((layout: WindowLayout) => void) | undefined;
  let resolve: ((layout: WindowLayout) => void) | undefined;
  const native: Platform = {
    ...platform,
    onWindowLayout: (listener: (layout: WindowLayout) => void) => { update = listener; return off; },
    windowLayout: () => new Promise<WindowLayout>(done => { resolve = done; }),
  };
  const { result, unmount } = renderHook(() => useSplitLayout(native));
  const resizeListener = addWindow.mock.calls.find(([name]) => name === "resize")?.[1];
  expect(resizeListener).toBeTypeOf("function");
  expect(addViewport).toHaveBeenCalledWith("resize", resizeListener);
  unmount();
  expect(removeWindow).toHaveBeenCalledWith("resize", resizeListener);
  expect(removeViewport).toHaveBeenCalledWith("resize", resizeListener);
  expect(off).toHaveBeenCalledOnce();
  const previous = result.current;
  await act(async () => {
    update?.(unavailableWindowLayout(915, 412));
    resolve?.(unavailableWindowLayout(915, 412));
    await Promise.resolve();
  });
  expect(result.current).toBe(previous);
});
