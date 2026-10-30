// SPDX-License-Identifier: Apache-2.0
import { createRef } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ControlTarget, InputAction } from "../../protocol/types";
import { LivePicture } from "./LivePicture";

const target: ControlTarget = {
  target_id: "screen", kind: "output", label: "Display", native_ref: "screen",
  geometry: { width: 1920, height: 1080, scale: 1, rotation_degrees: 0, revision: 1 },
  capture: { state: "available" }, input: { state: "available" },
};
// The surface is 400 × 225 CSS pixels, so (100, 45) is a quarter across and a fifth down.
let clock = 0;
let send = vi.fn<(action: InputAction) => boolean>();
beforeEach(() => {
  clock = 1000;
  send = vi.fn(() => true);
  localStorage.clear();
  vi.spyOn(performance, "now").mockImplementation(() => clock);
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockReturnValue(
    { left: 0, top: 0, width: 400, height: 225, right: 400, bottom: 225, x: 0, y: 0, toJSON: () => ({}) },
  );
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.stubGlobal("PointerEvent", class extends MouseEvent {
    pointerId: number; isPrimary: boolean;
    constructor(type: string, init: PointerEventInit) { super(type, init); this.pointerId = init.pointerId ?? 1; this.isPrimary = init.isPrimary ?? true; }
  });
  HTMLElement.prototype.setPointerCapture = vi.fn();
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

const canvas = createRef<HTMLCanvasElement>();
const show = () => render(<LivePicture target={target} canvas={canvas} retry={() => undefined} enabled send={send}
  picture={{ at: Date.now(), revision: 1, connected: true, error: null }} />);
const surface = () => canvas.current?.parentElement?.parentElement as HTMLElement;
const touch = (kind: "Down" | "Move" | "Up", x: number, y: number, pointerId = 1) =>
  fireEvent[`pointer${kind}`](surface(), { pointerId, clientX: x, clientY: y });
const tap = (x: number, y: number) => {
  touch("Down", x, y);
  clock += 40;
  touch("Up", x, y);
};
const sent = () => send.mock.calls.map(([a]) => a);

it("starts as a touchscreen: a tap moves the pointer there and a second tap clicks", () => {
  show();
  expect(screen.getByRole("radio", { name: "Touch" }).getAttribute("aria-checked")).toBe("true");
  expect(document.querySelector(".picture-caption")?.textContent).toMatch(/^Touch/);
  tap(100, 45);
  expect(sent()).toEqual([{ action: "pointer_move", to: { x: 0.25, y: 0.2 } }]);
  clock += 150;
  // A second tap a little off the first still clicks where the pointer is.
  tap(110, 50);
  expect(sent()[1]).toEqual({ action: "click", button: "left", at: { x: 0.25, y: 0.2 }, count: 1 });
  // A third makes the computer see a double click.
  clock += 150;
  tap(100, 45);
  expect(sent()).toHaveLength(3);
  expect(sent()[2]).toMatchObject({ action: "click", button: "left" });
  // A tap after a pause only moves the pointer again.
  clock += 1000;
  tap(200, 90);
  expect(sent()[3]).toEqual({ action: "pointer_move", to: { x: 0.5, y: 0.4 } });
});

it("right-clicks the next click only after Right click is chosen", () => {
  show();
  const right = screen.getByRole("button", { name: "Right click" });
  fireEvent.click(right);
  expect(right.getAttribute("aria-pressed")).toBe("true");
  expect(screen.getByText("Touch · next click right")).toBeTruthy();
  tap(100, 45);
  clock += 150;
  tap(100, 45);
  expect(sent()[1]).toEqual({ action: "click", button: "right", at: { x: 0.25, y: 0.2 }, count: 1 });
  expect(right.getAttribute("aria-pressed")).toBe("false");
  clock += 1000;
  tap(100, 45);
  clock += 150;
  tap(100, 45);
  expect(sent()[3]).toMatchObject({ action: "click", button: "left" });
});

it("drags with the button held from where the finger landed to where it lifts", () => {
  show();
  touch("Down", 100, 45);
  // Not yet: this may be the first finger of a pinch.
  clock += 50;
  touch("Move", 110, 45);
  expect(sent()).toEqual([]);
  clock += 100;
  touch("Move", 200, 90);
  touch("Up", 200, 90);
  expect(sent()).toEqual([
    { action: "pointer_move", to: { x: 0.25, y: 0.2 } },
    { action: "pointer_button", button: "left", pressed: true, at: { x: 0.25, y: 0.2 } },
    { action: "pointer_move", to: { x: 0.5, y: 0.4 } },
    { action: "pointer_button", button: "left", pressed: false, at: { x: 0.5, y: 0.4 } },
  ]);
  // A quick flick is a whole drag too.
  send.mockClear();
  clock += 1000;
  touch("Down", 100, 45);
  clock += 30;
  touch("Move", 300, 45);
  touch("Up", 300, 45);
  expect(sent().map((a) => a.action)).toEqual(["pointer_move", "pointer_button", "pointer_move", "pointer_button"]);
  expect(sent()[2]).toEqual({ action: "pointer_move", to: { x: 0.75, y: 0.2 } });
});

it("pinches the picture larger without sending any input", () => {
  show();
  touch("Down", 100, 100, 1);
  touch("Down", 200, 100, 2);
  clock += 30;
  touch("Move", 260, 100, 2);
  expect(canvas.current?.parentElement?.style.transform).toMatch(/scale\(1\.6\)$/);
  touch("Up", 100, 100, 1);
  touch("Up", 260, 100, 2);
  expect(sent()).toEqual([]);
});

it("slides two fingers to scroll, moving a zoomed picture to its edge first", () => {
  show();
  touch("Down", 100, 100, 1);
  touch("Down", 200, 100, 2);
  touch("Move", 100, 60, 1);
  // The page follows the fingers: up by 20 phone pixels is 96 down the computer's.
  expect(sent()).toHaveLength(1);
  expect(sent()[0]).toMatchObject({ action: "scroll", at: { x: 0.375 } });
  const first = sent()[0] as Extract<InputAction, { action: "scroll" }>;
  expect(first.dy).toBeCloseTo(96);
  expect(first.dx).toBeCloseTo(0);
  touch("Up", 100, 60, 1);
  touch("Up", 200, 100, 2);

  send.mockClear();
  fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
  touch("Down", 100, 100, 1);
  touch("Down", 200, 100, 2);
  touch("Move", 100, 60, 1);
  expect(canvas.current?.parentElement?.style.transform).toBe("translate(0px, -20px) scale(1.5)");
  expect(sent()).toEqual([]);
  // The picture's bottom edge stops it 112.5 up; the rest scrolls.
  touch("Move", 100, -140, 1);
  expect(canvas.current?.parentElement?.style.transform).toBe("translate(0px, -112.5px) scale(1.5)");
  const rest = sent()[0] as Extract<InputAction, { action: "scroll" }>;
  expect(rest.action).toBe("scroll");
  expect(rest.dy).toBeCloseTo(24);
});

it("keeps Touchpad as a choice this device remembers", () => {
  show();
  fireEvent.click(screen.getByRole("radio", { name: "Touchpad" }));
  expect(document.querySelector(".picture-caption")?.textContent).toMatch(/^Touchpad/);
  expect(screen.getByRole("button", { name: "Remote drag" })).toBeTruthy();
  expect(localStorage.getItem("control.pointer-mode")).toBe("touchpad");
  // A slide moves the pointer by the finger's travel; a tap clicks.
  touch("Down", 100, 45);
  touch("Move", 140, 45);
  touch("Up", 140, 45);
  expect(sent()[0]).toEqual({ action: "pointer_move", to: { x: 0.6, y: 0.5 } });
  cleanup();
  show();
  expect(screen.getByRole("radio", { name: "Touchpad" }).getAttribute("aria-checked")).toBe("true");
});

it("full screen works the same, and two fingers tapped together right-click between them", () => {
  render(<LivePicture full target={target} canvas={canvas} retry={() => undefined} enabled send={send}
    picture={{ at: Date.now(), revision: 1, connected: true, error: null }} />);
  tap(100, 45);
  clock += 150;
  tap(100, 45);
  expect(sent()).toEqual([
    { action: "pointer_move", to: { x: 0.25, y: 0.2 } },
    { action: "click", button: "left", at: { x: 0.25, y: 0.2 }, count: 1 },
  ]);
  send.mockClear();
  clock += 1000;
  touch("Down", 100, 45, 1);
  touch("Down", 140, 45, 2);
  clock += 60;
  touch("Up", 100, 45, 1);
  touch("Up", 140, 45, 2);
  expect(sent()).toEqual([{ action: "click", button: "right", at: { x: 0.3, y: 0.2 }, count: 1 }]);
});
