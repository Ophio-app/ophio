// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import {
  FIT,
  fitTransform,
  toNormalized,
  toViewport,
  hitTest,
  labelPosition,
  moveShape,
} from "./canvas";
import type { MarkShape } from "../../protocol/types";
describe("normalized source coordinates", () => {
  it.each([
    [360, 240, FIT, 1],
    [1280, 800, { zoom: 3, panX: 0.2, panY: -0.4 }, 2],
    [800, 360, { zoom: 1.4, panX: -0.15, panY: 0.05 }, 3],
  ] as const)(
    "survive fit, resize, zoom, pan and DPR (%s × %s)",
    (w, h, view, dpr) => {
      const t = fitTransform(w, h, 2560, 1440, view),
        source = { x: 0.43, y: 0.61 },
        screen = toViewport(source, t);
      const result = toNormalized(
        { x: screen.x * dpr, y: screen.y * dpr },
        t,
        dpr,
      );
      expect(result.x).toBeCloseTo(source.x);
      expect(result.y).toBeCloseTo(source.y);
    },
  );
  it("clamps drawing to intrinsic image bounds", () => {
    expect(
      toNormalized({ x: -1000, y: 1000 }, fitTransform(360, 300, 200, 100)),
    ).toEqual({ x: 0, y: 1 });
  });
});
describe("shape interaction", () => {
  const rect: MarkShape = {
    kind: "rect",
    rect: { x: 0.2, y: 0.3, w: 0.3, h: 0.2 },
  };
  it("selects rectangles, ellipse interiors, strokes, arrows and pins", () => {
    expect(hitTest(rect, { x: 0.3, y: 0.4 })).toBe(true);
    expect(hitTest(rect, { x: 0.9, y: 0.8 })).toBe(false);
    expect(hitTest({ ...rect, kind: "ellipse" }, { x: 0.2, y: 0.3 }, 0)).toBe(
      false,
    );
    expect(hitTest({ ...rect, kind: "ellipse" }, { x: 0.35, y: 0.4 })).toBe(
      true,
    );
    const from = { x: 0.1, y: 0.1, t: null },
      to = { x: 0.9, y: 0.9, t: null };
    expect(hitTest({ kind: "arrow", from, to }, { x: 0.5, y: 0.5 })).toBe(true);
    expect(
      hitTest({ kind: "stroke", points: [from, to] }, { x: 0.5, y: 0.7 }),
    ).toBe(false);
    expect(hitTest({ kind: "pin", at: from }, { x: 0.11, y: 0.1 })).toBe(true);
  });
  it("keeps labels legible inside all image edges", () => {
    expect(
      labelPosition(
        { kind: "pin", at: { x: 1, y: 0, t: null } },
        { x: 0.1, y: 0.1 },
      ),
    ).toEqual({ x: 0.9, y: 0.1 });
  });
  it("moves a whole mark without distorting or moving it outside the image", () => {
    expect(moveShape(rect, { x: 2, y: -2 })).toEqual({
      kind: "rect",
      rect: { x: 0.7, y: 0, w: 0.3, h: 0.2 },
    });
    expect(rect.rect.x).toBe(0.2);
  });
});
