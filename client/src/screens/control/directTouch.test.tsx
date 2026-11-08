// SPDX-License-Identifier: Apache-2.0
import { createRef } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ControlTarget, InputAction } from "../../protocol/types";
import { LivePicture } from "./LivePicture";
import { LONG_PRESS_MS } from "./gestures";

const target: ControlTarget = {
  target_id: "screen", kind: "output", label: "Display", native_ref: "screen",
  geometry: { width: 1920, height: 1080, scale: 1, rotation_degrees: 0, revision: 1 },
  capture: { state: "available" }, input: { state: "available" },
};
const canvas = createRef<HTMLCanvasElement>();
let send = vi.fn<(action: InputAction) => boolean>();
let clock = 1000;
let frames: Map<number, FrameRequestCallback>;
let frameId = 0;
const picture = { at: Date.now(), revision: 1, connected: true, error: null };
const show = (options: { direct?: boolean; enabled?: boolean; full?: boolean; target?: ControlTarget } = {}) => render(
  <LivePicture target={options.target ?? target} canvas={canvas} retry={() => undefined}
    enabled={options.enabled ?? true} full={options.full ?? false} send={send}
    touchBehavior={options.direct === false ? undefined : "direct"} picture={picture} />,
);
const surface = () => (document.querySelector(".picture-stage") ?? document.querySelector(".picture-surface")) as HTMLElement;
const event = (kind: "Down" | "Move" | "Up" | "Cancel", x = 100, y = 45, id = 1) => {
  fireEvent[`pointer${kind}`](surface(), { pointerId: id, clientX: x, clientY: y });
  const pending = [...frames.values()]; frames.clear();
  act(() => { for (const callback of pending) callback(clock); });
};
const advance = (ms: number) => { clock += ms; act(() => vi.advanceTimersByTime(ms)); };
const tap = (x = 100, y = 45) => { event("Down", x, y); advance(40); event("Up", x, y); };
const sent = () => send.mock.calls.map(([action]) => action);
const left = (x: number, y: number): InputAction => ({ action: "click", button: "left", at: { x, y }, count: 1 });

beforeEach(() => {
  vi.useFakeTimers(); clock = 1000; frameId = 0; frames = new Map();
  localStorage.clear(); send = vi.fn(() => true);
  vi.spyOn(performance, "now").mockImplementation(() => clock);
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => { frames.set(++frameId, cb); return frameId; });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => frames.delete(id));
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue(
    { left: 0, top: 0, width: 400, height: 225, right: 400, bottom: 225, x: 0, y: 0, toJSON: () => ({}) },
  );
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  HTMLElement.prototype.setPointerCapture = vi.fn();
});
afterEach(() => { cleanup(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

it("clicks at each tap immediately; two taps produce only two ordinary clicks", () => {
  show();
  tap(); advance(100); tap(104, 48);
  expect(sent()).toEqual([left(0.25, 0.2), left(0.25, 0.2)]);
  advance(600);
  expect(sent()).toHaveLength(2);
  tap(200, 90);
  expect(sent()[2]).toEqual(left(0.5, 0.4));
});

it("anchors a nearby second tap to the first accepted pixel even on an ultrawide display", () => {
  show({ target: { ...target, geometry: { ...target.geometry, width: 3440, height: 1440 } } });
  tap(100, 45); advance(100); tap(104, 48);
  expect(sent()).toEqual([left(0.25, 0.2), left(0.25, 0.2)]);
  // Four phone pixels would otherwise be over 30 desktop pixels at this scale.
  expect(Math.abs((104 / 400 - 0.25) * 3440)).toBeGreaterThan(30);
});

it("does not anchor to a refused click or to a previous right click", () => {
  show();
  send.mockImplementationOnce(() => false);
  tap(100, 45); advance(100); tap(104, 48);
  expect(sent()).toEqual([left(0.25, 0.2), left(0.26, 48 / 225)]);
  send.mockClear();
  advance(600);
  fireEvent.click(screen.getByRole("button", { name: "Right click" }));
  tap(100, 45); advance(100); tap(104, 48);
  expect(sent()).toEqual([
    { action: "click", button: "right", at: { x: 0.25, y: 0.2 }, count: 1 },
    left(0.26, 48 / 225),
  ]);
  send.mockClear(); advance(600);
  tap(100, 45);
  fireEvent.click(screen.getByRole("button", { name: "Right click" }));
  tap(104, 48);
  expect(sent()).toEqual([
    left(0.25, 0.2),
    { action: "click", button: "right", at: { x: 0.26, y: 48 / 225 }, count: 1 },
  ]);
});

it("retains the untouched position-first default and the next-right-click choice", () => {
  show({ direct: false });
  tap();
  expect(sent()).toEqual([{ action: "pointer_move", to: { x: 0.25, y: 0.2 } }]);
  fireEvent.click(screen.getByRole("button", { name: "Right click" }));
  tap();
  expect(sent()[1]).toEqual({ action: "click", button: "right", at: { x: 0.25, y: 0.2 }, count: 1 });
  cleanup(); send.mockClear(); show();
  fireEvent.click(screen.getByRole("button", { name: "Right click" }));
  tap(); tap();
  expect(sent()).toEqual([
    { action: "click", button: "right", at: { x: 0.25, y: 0.2 }, count: 1 },
    left(0.25, 0.2),
  ]);
});

it("right-clicks while held still, without a second click on release", () => {
  show(); event("Down"); advance(LONG_PRESS_MS - 1);
  expect(sent()).toEqual([]);
  advance(1);
  expect(sent()).toEqual([{ action: "click", button: "right", at: { x: 0.25, y: 0.2 }, count: 1 }]);
  event("Up"); advance(600);
  expect(sent()).toHaveLength(1);
  event("Down"); advance(LONG_PRESS_MS); event("Move", 200, 45); event("Up", 200, 45);
  expect(sent()).toHaveLength(2);
});

it("cancels long-press on drift and keeps short flicks as drags", () => {
  show(); event("Down"); advance(20); event("Move", 110, 45);
  advance(LONG_PRESS_MS); expect(sent()).toEqual([]);
  event("Up", 110, 45);
  expect(sent().map((a) => a.action)).toEqual(["pointer_move", "pointer_button", "pointer_move", "pointer_button"]);
  expect(sent()[1]).toMatchObject({ pressed: true });
  expect(sent()[3]).toMatchObject({ pressed: false });
});

it("preserves two-finger tap and pinch without a delayed single-finger click", () => {
  show(); event("Down", 100, 45, 1); event("Down", 200, 45, 2);
  advance(30); event("Up", 100, 45, 1); event("Up", 200, 45, 2);
  expect(sent()).toEqual([{ action: "click", button: "right", at: { x: 0.375, y: 0.2 }, count: 1 }]);
  advance(600); expect(sent()).toHaveLength(1);
  send.mockClear(); event("Down"); advance(LONG_PRESS_MS);
  event("Down", 200, 45, 2); event("Up", 200, 45, 2); event("Up");
  expect(sent()).toEqual([{ action: "click", button: "right", at: { x: 0.25, y: 0.2 }, count: 1 }]);
  send.mockClear(); event("Down", 100, 100, 1); event("Down", 200, 100, 2);
  event("Move", 260, 100, 2); event("Up", 100, 100, 1); event("Up", 260, 100, 2);
  expect(canvas.current?.parentElement?.style.transform).toMatch(/scale\(1\.6\)$/);
  expect(sent()).toEqual([]);
});

it("does not send a late click after cancellation, mode change, target change, disabling or unmount", () => {
  const ui = show();
  event("Down"); fireEvent.lostPointerCapture(surface(), { pointerId: 1 });
  advance(600); event("Up"); expect(sent()).toEqual([]);
  event("Down"); event("Cancel"); advance(600); event("Up"); expect(sent()).toEqual([]);
  event("Down"); fireEvent.click(screen.getByRole("radio", { name: "Touchpad" })); advance(600); event("Up"); expect(sent()).toEqual([]);
  fireEvent.click(screen.getByRole("radio", { name: "Touch" }));
  event("Down"); ui.rerender(<LivePicture target={{ ...target, target_id: "other" }} canvas={canvas} retry={() => undefined} enabled send={send} touchBehavior="direct" picture={picture} />);
  advance(600); event("Up"); expect(sent()).toEqual([]);
  event("Down"); ui.rerender(<LivePicture target={target} canvas={canvas} retry={() => undefined} enabled={false} send={send} touchBehavior="direct" picture={picture} />);
  advance(600); event("Up"); expect(sent()).toEqual([]);
  ui.rerender(<LivePicture target={target} canvas={canvas} retry={() => undefined} enabled send={send} touchBehavior="direct" picture={picture} />);
  event("Down"); ui.unmount(); advance(600); expect(sent()).toEqual([]);
});

it("refused drag press and refused long press never produce a click on release", () => {
  show();
  send.mockImplementation((a) => a.action !== "pointer_button" && a.action !== "click");
  event("Down"); advance(150); event("Move", 180, 45); event("Up", 180, 45);
  expect(sent().map((a) => a.action)).toEqual(["pointer_move", "pointer_button"]);
  send.mockClear(); event("Down"); advance(LONG_PRESS_MS); event("Up");
  expect(sent()).toEqual([{ action: "click", button: "right", at: { x: 0.25, y: 0.2 }, count: 1 }]);
});

it("a long press consumes the one-shot right-click choice", () => {
  show(); fireEvent.click(screen.getByRole("button", { name: "Right click" }));
  event("Down"); advance(LONG_PRESS_MS); event("Up");
  expect(screen.getByRole("button", { name: "Right click" }).getAttribute("aria-pressed")).toBe("false");
  tap(200, 90); expect(sent().at(-1)).toEqual(left(0.5, 0.4));
});
