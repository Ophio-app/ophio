// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Canvas } from "./Canvas";
import { FIT, fitTransform, toViewport } from "./canvas";
import type { Mark } from "../../protocol/types";
import type { SourceDraft } from "../../state/annotations";
vi.mock("./useSourceUrl", () => ({ useSourceUrl: () => ({ url: "blob:picture", error: null }) }));

// A strip cropped from a toolbar: ten times wider than tall.
const source: SourceDraft = { key: "S1", blobKey: "blob", artifactId: null, sha256: null, width: 2000, height: 200,
  mediaType: "image/png", durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 0, derivedFrom: null, derivation: null };
const pin = (id: string, x: number): Mark => ({ mark_id: id, label: id, source_key: "S1", color: 0, created_at: 0, deleted: false,
  shape: { kind: "pin", at: { x, y: 0.5, t: null } } });
beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.stubGlobal("PointerEvent", class extends MouseEvent {
    pointerId: number; isPrimary: boolean;
    constructor(type: string, init: PointerEventInit) { super(type, init); this.pointerId = init.pointerId ?? 1; this.isPrimary = init.isPrimary ?? true; }
  });
  SVGElement.prototype.setPointerCapture = vi.fn();
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it("tapping directly on a mark in a wide image selects that mark, not a neighbour", () => {
  const onSelect = vi.fn();
  render(<Canvas source={source} marks={[pin("A", 0.3), pin("B", 0.6)]} tool="stroke" selected={[]} viewport={FIT} clock={0}
    onAdd={() => {}} onMove={() => {}} onSelect={onSelect} onViewport={() => {}} />);
  // The canvas starts at 320 × 220 CSS pixels; A and B are ~96 px apart on screen.
  const t = fitTransform(320, 220, source.width, source.height, FIT);
  const a = toViewport({ x: 0.3, y: 0.5 }, t);
  fireEvent.pointerDown(screen.getByRole("img"), { pointerId: 1, isPrimary: true, clientX: a.x, clientY: a.y });
  fireEvent.pointerUp(screen.getByRole("img"), { pointerId: 1, isPrimary: true, clientX: a.x, clientY: a.y });
  expect(onSelect).toHaveBeenCalledExactlyOnceWith("A");
});
