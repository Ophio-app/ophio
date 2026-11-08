// SPDX-License-Identifier: Apache-2.0
import { createRef } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ControlTarget, InputAction } from "../../protocol/types";
import { LivePicture } from "./LivePicture";
import { targetPoint, visibleRegion } from "./gestures";

const target: ControlTarget = {
  target_id: "ultrawide", kind: "output", label: "Ultrawide", native_ref: "screen",
  geometry: { width: 3440, height: 1440, scale: 1, rotation_degrees: 0, revision: 1 },
  capture: { state: "available" }, input: { state: "available" },
};
const picture = { at: Date.now(), revision: 1, connected: true, error: null };
const rect = (left: number, top: number, width: number, height: number) =>
  ({ left, top, width, height, right: left + width, bottom: top + height, x: left, y: top, toJSON: () => ({}) }) as DOMRect;
let now = 1000;
let observers: { elements: Set<Element>; notify: () => void }[] = [];
const resized = (element: Element) => act(() => {
  for (const observer of [...observers]) if (observer.elements.has(element)) observer.notify();
});
beforeEach(() => {
  observers = [];
  localStorage.clear(); now = 1000;
  vi.spyOn(performance, "now").mockImplementation(() => now);
  vi.stubGlobal("ResizeObserver", class {
    elements = new Set<Element>();
    constructor(readonly notify: () => void) { observers.push(this); }
    observe(element: Element) { this.elements.add(element); }
    disconnect() { this.elements.clear(); }
  });
  vi.stubGlobal("PointerEvent", class extends MouseEvent {
    pointerId: number;
    constructor(type: string, init: PointerEventInit) { super(type, init); this.pointerId = init.pointerId ?? 1; }
  });
  HTMLElement.prototype.setPointerCapture = vi.fn();
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

function setup(width = 412, height = 915, enabled = true) {
  let box = rect(0, (height - width * 1440 / 3440) / 2, width, width * 1440 / 3440);
  let stageBox = rect(0, 0, width, height);
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    return this.classList.contains("picture-stage") ? stageBox : box;
  });
  const send = vi.fn<(_: InputAction) => boolean>(() => true);
  const onRegion = vi.fn();
  const canvas = createRef<HTMLCanvasElement>();
  const props = { full: true, target, canvas, picture, retry: () => {}, send, onRegion };
  const overlay = (tools: import("./LivePicture").PictureTools) => <>
    <button onClick={tools.zoomIn}>Zoom in</button>
    <button onClick={() => tools.setMode("touchpad")}>Touchpad</button>
  </>;
  const rendered = render(<LivePicture {...props} enabled={enabled} overlay={overlay} />);
  const stage = () => canvas.current!.closest(".picture-stage")!;
  const touch = (kind: "Down" | "Move" | "Up" | "Cancel" | "LostPointerCapture", x: number, y: number, pointerId = 1) => {
    const event = kind === "LostPointerCapture" ? "lostPointerCapture" : `pointer${kind}`;
    fireEvent[event as "pointerDown"](stage(), { pointerId, clientX: x, clientY: y });
  };
  return { box, canvas, send, onRegion, touch,
    resize: (nextHeight: number, hidden = false, nextWidth = width) => {
      const fittedWidth = Math.min(nextWidth, nextHeight * 3440 / 1440);
      const fittedHeight = fittedWidth * 1440 / 3440;
      stageBox = hidden ? rect(0, 0, 0, 0) : rect(0, 0, nextWidth, nextHeight);
      box = hidden ? rect(0, 0, 0, 0) : rect((nextWidth - fittedWidth) / 2, (nextHeight - fittedHeight) / 2, fittedWidth, fittedHeight);
      resized(stage());
    },
    rerender: (active: boolean) => rendered.rerender(<LivePicture {...props} enabled={active} overlay={overlay} />) };
}

describe.each([["portrait", 412, 915], ["landscape", 915, 412]] as const)("ultrawide %s", (_name, width, height) => {
  it.each([1, 2, 4])("a drag reaches the visible desktop top at %sx zoom", (zoom) => {
    const { box, touch, send } = setup(width, height);
    for (let z = 1; z < zoom; z += 0.5) fireEvent.click(screen.getByText("Zoom in"));
    // Zoom is centred. Use the first visible pixel (including the 34px desktop
    // bar when it fits), not the old unzoomed picture's top edge.
    const panY = box.height * (1 - zoom) / 2;
    const top = Math.max(0, box.top + panY) + 1;
    touch("Down", width / 2, height / 2);
    now += 200;
    touch("Move", width / 2, top);
    touch("Up", width / 2, top);
    const expectedY = (top - box.top - panY) / (box.height * zoom);
    const moves = send.mock.calls.map(([a]) => a).filter(a => a.action === "pointer_move");
    expect(moves.at(-1)).toMatchObject({ action: "pointer_move", to: { x: 0.5, y: expect.closeTo(expectedY) } });
    if (_name === "portrait") expect(expectedY * 1440).toBeLessThan(34);
  });

  it.each([1, 2, 4])("touchpad reaches every desktop edge at %sx zoom", (zoom) => {
    const { touch, send } = setup(width, height);
    fireEvent.click(screen.getByText("Touchpad"));
    for (let z = 1; z < zoom; z += 0.5) fireEvent.click(screen.getByText("Zoom in"));
    for (const [x, y] of [[-10000, -10000], [10000, 10000]] as const) {
      touch("Down", width / 2, height / 2); now += 200;
      touch("Move", x, y); touch("Up", x, y);
    }
    const moves = send.mock.calls.map(([a]) => a).filter(a => a.action === "pointer_move");
    expect(moves).toEqual([
      { action: "pointer_move", to: { x: 0, y: 0 } },
      { action: "pointer_move", to: { x: 1, y: 1 } },
    ]);
  });
});

it("requests all visible zoomed pixels in the portrait letterbox, not only the middle strip", () => {
  const { onRegion } = setup();
  fireEvent.click(screen.getByText("Zoom in"));
  fireEvent.click(screen.getByText("Zoom in"));
  const region = onRegion.mock.calls.at(-1)![0];
  expect(region).toMatchObject({ y: 0, height: 1 });
  expect(region.width).toBeLessThan(1);
});

it("ignores taps in black letterboxing, while dragging past the desktop clamps to its edge", () => {
  const { touch, send } = setup();
  touch("Down", 200, 200); touch("Up", 200, 200);
  expect(send).not.toHaveBeenCalled();
  touch("Down", 200, 457); now += 200;
  touch("Move", 200, 100); touch("Up", 200, 100);
  expect(send.mock.calls.map(([a]) => a).filter(a => a.action === "pointer_move").at(-1))
    .toMatchObject({ action: "pointer_move", to: { y: 0 } });
});

it("losing capture cancels the contact and releases a held drag before the next touch", () => {
  const { touch, send } = setup();
  touch("Down", 200, 457); now += 200; touch("Move", 220, 470);
  touch("LostPointerCapture", 220, 470);
  expect(send.mock.calls.at(-1)![0]).toMatchObject({ action: "pointer_button", pressed: false });
  send.mockClear();
  touch("Down", 100, 400, 2); touch("Up", 100, 400, 2);
  expect(send.mock.calls).toHaveLength(1);
  expect(send.mock.calls[0]![0]).toMatchObject({ action: "pointer_move" });
});

it("pausing input discards a gesture so it cannot resume as a remote drag", () => {
  const { touch, send, rerender } = setup();
  touch("Down", 200, 457); now += 200; touch("Move", 220, 470);
  rerender(false);
  expect(send.mock.calls.at(-1)![0]).toMatchObject({ action: "pointer_button", pressed: false });
  rerender(true); send.mockClear();
  touch("Move", 250, 490); touch("Up", 250, 490);
  expect(send).not.toHaveBeenCalled();
});

it.each([1, 2])("a refused drag start action %s is never replayed when transport recovers before finger-up", (refused) => {
  const { touch, send } = setup();
  if (refused === 2) send.mockReturnValueOnce(true);
  send.mockReturnValueOnce(false);
  touch("Down", 200, 457); now += 200; touch("Move", 220, 470);
  const calls = send.mock.calls.length;
  touch("Up", 220, 470);
  expect(calls).toBe(refused);
  expect(send).toHaveBeenCalledTimes(calls);
});

it("a stage-only resize cancels a drag and refreshes the requested visible region", () => {
  const { touch, send, resize, onRegion } = setup();
  for (let i = 0; i < 6; i++) fireEvent.click(screen.getByText("Zoom in"));
  const oldRegion = onRegion.mock.calls.at(-1)![0];
  touch("Down", 200, 457); now += 200; touch("Move", 220, 470);
  resize(300); // Picture's width and height stay fixed; only its position moves.
  expect(send.mock.calls.at(-1)![0]).toMatchObject({ action: "pointer_button", pressed: false });
  expect(onRegion.mock.calls.at(-1)![0].height).toBeLessThan(oldRegion.height);
  send.mockClear(); touch("Move", 250, 150); touch("Up", 250, 150);
  expect(send).not.toHaveBeenCalled();
});

it("hiding and returning to a retained route preserves its zoom and pan", () => {
  const { canvas, resize } = setup();
  fireEvent.click(screen.getByText("Zoom in"));
  const before = canvas.current!.parentElement!.style.transform;
  expect(before).not.toContain("translate(0px, 0px)");
  resize(0, true);
  expect(canvas.current!.parentElement!.style.transform).toBe(before);
  resize(915);
  expect(canvas.current!.parentElement!.style.transform).toBe(before);
});

it("clamps transformed target coordinates without depending on device pixel ratio", () => {
  for (const dpr of [1, 2.625, 3]) {
    const view = { left: 0, top: 371, width: 412, height: 172, zoom: 2, pan: { x: -206, y: -86 }, dpr };
    expect(targetPoint({ x: 206, y: 285 }, view, true)).toEqual({ x: 0.5, y: 0 });
    expect(targetPoint({ x: 206, y: 200 }, view)).toBeNull();
    expect(targetPoint({ x: 206, y: 200 }, view, true)).toEqual({ x: 0.5, y: 0 });
  }
  expect(visibleRegion({ zoom: 2, pan: { x: -206, y: -86 } }, { width: 412, height: 172 },
    { left: 0, top: -371, width: 412, height: 915 })).toMatchObject({ y: 0, height: 1 });
});

it("keeps the known pointer visible when the keyboard narrows a zoomed picture", () => {
  const { touch, resize, canvas, send } = setup();
  touch("Down", 41.2, 457); touch("Up", 41.2, 457);
  fireEvent.click(screen.getByText("Zoom in")); fireEvent.click(screen.getByText("Zoom in"));
  send.mockClear(); resize(300, false, 200);
  const transform = canvas.current!.parentElement!.style.transform;
  const panX = Number(/translate\(([-.\d]+)px/.exec(transform)![1]);
  const pointerX = 0.1 * 200 * 2 + panX;
  expect(pointerX).toBeGreaterThanOrEqual(0); expect(pointerX).toBeLessThanOrEqual(200);
  expect(send).not.toHaveBeenCalled();
});
