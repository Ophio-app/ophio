// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, renderHook, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useLandscape } from "../screens/control/FullControl";
import { useCrampedLayout, useNarrowLayout, useSidewaysLayout, useTightLayout } from "./keyboard";
import { PaneContext, PaneFrame, usePane, type PaneScope } from "./pane";

const sizes = new Map<string, { width: number; height: number }>();
const observers: Array<{ callback: ResizeObserverCallback; target: Element | null }> = [];
function resize() {
  act(() => {
    for (const observer of observers) if (observer.target?.classList.contains("pane")) observer.callback([], {} as ResizeObserver);
  });
}
function Layout() {
  const pane = usePane();
  const value = { width: pane?.width, height: pane?.height, tight: useTightLayout(), narrow: useNarrowLayout(), cramped: useCrampedLayout(), sideways: useSidewaysLayout(), landscape: useLandscape() };
  return <output data-testid={pane?.id ?? "legacy"}>{JSON.stringify(value)}</output>;
}
const state = (id: string) => JSON.parse(screen.getByTestId(id).textContent!);
const frame = (id: string, active = false, visible = true, split = true, activate = () => {}) =>
  <PaneFrame id={id} label={id === "left" ? "Left side" : "Right side"} active={active} visible={visible} split={split} onActivate={activate}><Layout /><button type="button">{id} control</button></PaneFrame>;

beforeEach(() => {
  sizes.clear(); observers.length = 0;
  document.documentElement.style.fontSize = "16px";
  Object.defineProperties(window, { innerWidth: { configurable: true, value: 690 }, innerHeight: { configurable: true, value: 830 } });
  vi.stubGlobal("ResizeObserver", class {
    entry = { callback: (() => {}) as ResizeObserverCallback, target: null as Element | null };
    constructor(callback: ResizeObserverCallback) { this.entry.callback = callback; observers.push(this.entry); }
    observe(target: Element) { this.entry.target = target; }
    disconnect() { this.entry.target = null; }
  });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    return { ...sizes.get(this.dataset.paneId ?? ""), x: 0, y: 0, top: 0, left: 0 } as DOMRect;
  });
});
afterEach(() => {
  cleanup();
  document.documentElement.style.fontSize = "";
  vi.restoreAllMocks(); vi.unstubAllGlobals();
});

it("classifies each 345px Fold pane instead of the 690px window", () => {
  sizes.set("left", { width: 345, height: 830 });
  sizes.set("right", { width: 345, height: 830 });
  render(<>{frame("left", true)}{frame("right")}</>);
  for (const id of ["left", "right"]) {
    expect(state(id)).toMatchObject({ width: 345, height: 830, narrow: true, tight: false, cramped: false, sideways: false, landscape: false });
  }
  expect(document.documentElement.classList.contains("app-narrow")).toBe(false);
  expect(screen.getByRole("region", { name: "Left side" }).classList.contains("app-narrow")).toBe(true);
  expect(screen.getByRole("region", { name: "Right side" }).hasAttribute("inert")).toBe(false);
  // Mounting both surfaces creates only the one shared text-size probe.
  expect(document.querySelectorAll('div[aria-hidden="true"][style*="1rem"]')).toHaveLength(1);
});

it("keeps two visible panes live and activates without moving focus", () => {
  sizes.set("left", { width: 345, height: 830 }); sizes.set("right", { width: 345, height: 830 });
  const activate = vi.fn();
  render(<>{frame("left", true)}{frame("right", false, true, true, activate)}</>);
  const left = screen.getByRole("button", { name: "left control" });
  const right = screen.getByRole("button", { name: "right control" });
  left.focus();
  fireEvent.pointerDown(right);
  expect(activate).toHaveBeenCalledOnce();
  expect(document.activeElement).toBe(left);
  right.focus();
  expect(activate).toHaveBeenCalledTimes(2);
});

it("makes a hidden pane inert without discarding its last measured size", () => {
  sizes.set("right", { width: 345, height: 830 });
  const activate = vi.fn();
  const view = render(frame("right", true, true, true, activate));
  sizes.set("right", { width: 0, height: 0 });
  view.rerender(frame("right", false, false, false, activate)); resize();
  const region = view.container.querySelector(".pane")!;
  expect(region.hasAttribute("inert")).toBe(true);
  expect(region.getAttribute("aria-hidden")).toBe("true");
  expect(state("right")).toMatchObject({ width: 345, height: 830 });
  fireEvent.pointerDown(region);
  expect(activate).not.toHaveBeenCalled();
});

it("responds to each pane's dimensions and to 200 percent text", () => {
  sizes.set("left", { width: 345, height: 830 }); sizes.set("right", { width: 700, height: 400 });
  render(<>{frame("left", true)}{frame("right")}</>);
  expect(state("left")).toMatchObject({ tight: false, sideways: false, landscape: false });
  expect(state("right")).toMatchObject({ tight: true, sideways: true, landscape: true });
  document.documentElement.style.fontSize = "32px";
  act(() => { window.dispatchEvent(new Event("resize")); });
  expect(state("left")).toMatchObject({ tight: true, narrow: true, cramped: false });
  expect(screen.getByRole("region", { name: "Left side" }).classList.contains("app-tight")).toBe(true);
  sizes.set("left", { width: 345, height: 250 }); resize();
  expect(state("left")).toMatchObject({ cramped: true });
});

it("preserves the legacy window classification and device orientation without a pane", () => {
  vi.stubGlobal("matchMedia", () => ({ matches: true, addEventListener() {}, removeEventListener() {} }));
  const { result } = renderHook(() => ({ pane: usePane(), narrow: useNarrowLayout(), sideways: useSidewaysLayout(), landscape: useLandscape() }));
  act(() => { window.dispatchEvent(new Event("resize")); });
  expect(result.current).toEqual({ pane: null, narrow: false, sideways: false, landscape: true });
});

it("does not subscribe each pane to device orientation changes", () => {
  const subscribe = vi.fn();
  vi.stubGlobal("matchMedia", () => ({ matches: true, addEventListener: subscribe, removeEventListener() {} }));
  const scope: PaneScope = { id: "test", root: document.createElement("div"), active: true, visible: true, split: true, width: 345, height: 830 };
  const wrapper = ({ children }: { children: React.ReactNode }) => <PaneContext.Provider value={scope}>{children}</PaneContext.Provider>;
  const { result } = renderHook(useLandscape, { wrapper });
  expect(result.current).toBe(false);
  expect(subscribe).not.toHaveBeenCalled();
});
