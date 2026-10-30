// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Canvas } from "./Canvas";
import { FIT } from "./canvas";
import type { SourceDraft } from "../../state/annotations";
vi.mock("./useSourceUrl", () => ({ useSourceUrl: () => ({ url: "blob:picture", error: null }) }));
const source: SourceDraft = { key: "S1", blobKey: "blob", artifactId: null, sha256: null, width: 320, height: 220,
  mediaType: "image/png", durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 0, derivedFrom: null, derivation: null };
beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.stubGlobal("PointerEvent", class extends MouseEvent {
    pointerId: number; isPrimary: boolean;
    constructor(type: string, init: PointerEventInit) { super(type, init); this.pointerId = init.pointerId ?? 1; this.isPrimary = init.isPrimary ?? true; }
  });
  SVGElement.prototype.setPointerCapture = vi.fn();
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
it.each([false, true])("draws after an abandoned pointer (lost capture: %s)", lost => {
  const onAdd = vi.fn(), onViewport = vi.fn();
  render(<Canvas source={source} marks={[]} tool="rect" selected={[]} viewport={FIT} clock={0}
    onAdd={onAdd} onMove={() => {}} onSelect={() => {}} onViewport={onViewport} />);
  const canvas = screen.getByRole("img");
  fireEvent.pointerDown(canvas, { pointerId: 1, isPrimary: true, clientX: 50, clientY: 50 });
  if (lost) fireEvent.lostPointerCapture(canvas, { pointerId: 1 });
  fireEvent.pointerDown(canvas, { pointerId: 2, isPrimary: true, clientX: 60, clientY: 60 });
  fireEvent.pointerMove(canvas, { pointerId: 2, isPrimary: true, clientX: 120, clientY: 100 });
  fireEvent.pointerUp(canvas, { pointerId: 2, isPrimary: true, clientX: 120, clientY: 100 });
  expect(onAdd).toHaveBeenCalledExactlyOnceWith({ kind: "rect", rect: { x: 60 / 320, y: 60 / 220, w: 60 / 320, h: 40 / 220 } });
  expect(onViewport).not.toHaveBeenCalled();
});
it("retains a genuine second touch for pinch zoom", () => {
  const onAdd = vi.fn(), onViewport = vi.fn();
  render(<Canvas source={source} marks={[]} tool="rect" selected={[]} viewport={FIT} clock={0}
    onAdd={onAdd} onMove={() => {}} onSelect={() => {}} onViewport={onViewport} />);
  const canvas = screen.getByRole("img");
  fireEvent.pointerDown(canvas, { pointerId: 1, isPrimary: true, clientX: 50, clientY: 50 });
  fireEvent.pointerDown(canvas, { pointerId: 2, isPrimary: false, clientX: 100, clientY: 100 });
  fireEvent.pointerMove(canvas, { pointerId: 2, isPrimary: false, clientX: 150, clientY: 150 });
  fireEvent.pointerUp(canvas, { pointerId: 2, isPrimary: false });
  expect(onViewport).toHaveBeenCalledWith(expect.objectContaining({ zoom: 2 }));
  expect(onAdd).not.toHaveBeenCalled();
});
it("keeps a box begun on the letterbox, clamped to the image", () => {
  const onAdd = vi.fn();
  // A wide picture in the 320 × 220 canvas leaves 55 px of letterbox above and below it.
  render(<Canvas source={{ ...source, height: 110 }} marks={[]} tool="rect" selected={[]} viewport={FIT} clock={0}
    onAdd={onAdd} onMove={() => {}} onSelect={() => {}} onViewport={() => {}} />);
  const canvas = screen.getByRole("img");
  fireEvent.pointerDown(canvas, { pointerId: 1, isPrimary: true, clientX: 40, clientY: 20 });
  fireEvent.pointerMove(canvas, { pointerId: 1, isPrimary: true, clientX: 120, clientY: 100 });
  fireEvent.pointerUp(canvas, { pointerId: 1, isPrimary: true, clientX: 120, clientY: 100 });
  expect(onAdd).toHaveBeenCalledExactlyOnceWith({ kind: "rect", rect: { x: 40 / 320, y: 0, w: 80 / 320, h: 45 / 110 } });
});
it("selects a mark at the image's edge only from a touch near it", () => {
  const onSelect = vi.fn();
  const edge = { mark_id: "top", label: "A", color: 0, source_key: "S1", created_at: 0, deleted: false,
    shape: { kind: "rect" as const, rect: { x: 0.25, y: 0, w: 0.5, h: 0.2 } } };
  render(<Canvas source={{ ...source, height: 110 }} marks={[edge]} tool="rect" selected={[]} viewport={FIT} clock={0}
    onAdd={() => {}} onMove={() => {}} onSelect={onSelect} onViewport={() => {}} />);
  const canvas = screen.getByRole("img");
  // 45 px above the picture, on the letterbox.
  fireEvent.pointerDown(canvas, { pointerId: 1, isPrimary: true, clientX: 160, clientY: 10 });
  fireEvent.pointerUp(canvas, { pointerId: 1, isPrimary: true, clientX: 160, clientY: 10 });
  expect(onSelect).not.toHaveBeenCalledWith("top");
  // 10 px above its top edge.
  fireEvent.pointerDown(canvas, { pointerId: 2, isPrimary: true, clientX: 160, clientY: 45 });
  fireEvent.pointerUp(canvas, { pointerId: 2, isPrimary: true, clientX: 160, clientY: 45 });
  expect(onSelect).toHaveBeenLastCalledWith("top");
});
