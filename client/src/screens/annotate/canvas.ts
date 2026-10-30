// SPDX-License-Identifier: Apache-2.0
import type { MarkShape, NormRect, StrokePoint } from "../../protocol/types";
export type Point = { x: number; y: number };
export type Viewport = { zoom: number; panX: number; panY: number };
export const FIT: Viewport = { zoom: 1, panX: 0, panY: 0 };
export const clamp = (n: number, low = 0, high = 1): number =>
  Math.min(high, Math.max(low, n));
export type Transform = { x: number; y: number; width: number; height: number };
/** The part of the canvas the picture fits into at FIT, in canvas pixels:
 * full screen, the space the controls leave clear. */
export type Room = Transform;
export function fitTransform(
  width: number,
  height: number,
  sourceWidth: number,
  sourceHeight: number,
  v: Viewport = FIT,
  room: Room = { x: 0, y: 0, width, height },
): Transform {
  const scale =
    Math.min(
      room.width / Math.max(1, sourceWidth),
      room.height / Math.max(1, sourceHeight),
    ) * v.zoom;
  const w = sourceWidth * scale,
    h = sourceHeight * scale;
  return {
    x: room.x + (room.width - w) / 2 + v.panX * width,
    y: room.y + (room.height - h) / 2 + v.panY * height,
    width: w,
    height: h,
  };
}
/** Zoom kept between whole picture and 8×, and pan kept to where the picture
 * still fills the room it was fitted to, as a photo viewer does. */
export function clampView(
  v: Viewport,
  width: number,
  height: number,
  sourceWidth: number,
  sourceHeight: number,
  room: Room = { x: 0, y: 0, width, height },
): Viewport {
  const zoom = clamp(v.zoom, 1, 8),
    t = fitTransform(width, height, sourceWidth, sourceHeight, { zoom, panX: 0, panY: 0 }, room),
    x = Math.max(0, (t.width - room.width) / 2) / Math.max(1, width),
    y = Math.max(0, (t.height - room.height) / 2) / Math.max(1, height);
  return { zoom, panX: clamp(v.panX, -x, x), panY: clamp(v.panY, -y, y) };
}
/** The view at `zoom` that keeps the picture point under `from` in `t` under
 * `to`: a pinch's fingers stay on what they first touched. */
export function zoomAbout(
  v: Viewport,
  t: Transform,
  from: Point,
  to: Point,
  zoom: number,
  width: number,
  height: number,
): Viewport {
  const k = zoom / v.zoom,
    w = t.width * k,
    h = t.height * k,
    midX = t.x + t.width / 2 - v.panX * width,
    midY = t.y + t.height / 2 - v.panY * height;
  return {
    zoom,
    panX: (to.x - ((from.x - t.x) / t.width) * w - (midX - w / 2)) / Math.max(1, width),
    panY: (to.y - ((from.y - t.y) / t.height) * h - (midY - h / 2)) / Math.max(1, height),
  };
}
/** Input is CSS pixels; DPR applies only when converting a raster's backing pixels. */
export function toNormalized(p: Point, t: Transform, dpr = 1): Point {
  return {
    x: clamp((p.x / dpr - t.x) / t.width),
    y: clamp((p.y / dpr - t.y) / t.height),
  };
}
export function toViewport(p: Point, t: Transform): Point {
  return { x: t.x + p.x * t.width, y: t.y + p.y * t.height };
}
export function rectBetween(a: Point, b: Point): NormRect {
  return {
    x: Math.min(a.x, b.x),
    y: Math.min(a.y, b.y),
    w: Math.abs(a.x - b.x),
    h: Math.abs(a.y - b.y),
  };
}
export function bounds(s: MarkShape): NormRect {
  if (s.kind === "rect" || s.kind === "ellipse") return s.rect;
  const p =
    s.kind === "stroke" ? s.points : s.kind === "pin" ? [s.at] : [s.from, s.to];
  const xs = p.map((v) => v.x),
    ys = p.map((v) => v.y);
  const x = Math.min(...xs),
    y = Math.min(...ys);
  return p.length
    ? { x, y, w: Math.max(...xs) - x, h: Math.max(...ys) - y }
    : { x: 0, y: 0, w: 0, h: 0 };
}
function lineDistance(p: Point, a: Point, b: Point): number {
  const dx = b.x - a.x,
    dy = b.y - a.y,
    len = dx * dx + dy * dy;
  const t = len ? clamp(((p.x - a.x) * dx + (p.y - a.y) * dy) / len) : 0;
  return Math.hypot(p.x - a.x - t * dx, p.y - a.y - t * dy);
}
export function hitTest(s: MarkShape, p: Point, tolerance = 0.025): boolean {
  if (s.kind === "pin")
    return Math.hypot(p.x - s.at.x, p.y - s.at.y) <= tolerance;
  if (s.kind === "arrow") return lineDistance(p, s.from, s.to) <= tolerance;
  if (s.kind === "stroke")
    return s.points.some(
      (b, i) =>
        lineDistance(p, s.points[Math.max(0, i - 1)] ?? b, b) <= tolerance,
    );
  const r = s.rect;
  if (s.kind === "ellipse")
    return (
      Math.hypot(
        (p.x - r.x - r.w / 2) / (r.w / 2 + tolerance),
        (p.y - r.y - r.h / 2) / (r.h / 2 + tolerance),
      ) <= 1
    );
  return (
    p.x >= r.x - tolerance &&
    p.x <= r.x + r.w + tolerance &&
    p.y >= r.y - tolerance &&
    p.y <= r.y + r.h + tolerance
  );
}
/** How far `p` is from where the shape is drawn, in the shape's own units:
 * a box's or ellipse's outline, a pin's dot, a line. Zero on it, and inside
 * a pin. */
export function outlineDistance(s: MarkShape, p: Point, pinRadius = 0): number {
  if (s.kind === "pin")
    return Math.max(0, Math.hypot(p.x - s.at.x, p.y - s.at.y) - pinRadius);
  if (s.kind === "arrow") return lineDistance(p, s.from, s.to);
  if (s.kind === "stroke")
    return Math.min(
      ...s.points.map((b, i) =>
        lineDistance(p, s.points[Math.max(0, i - 1)] ?? b, b),
      ),
      Infinity,
    );
  const r = s.rect;
  if (s.kind === "ellipse") {
    const rx = r.w / 2,
      ry = r.h / 2,
      dx = p.x - r.x - rx,
      dy = p.y - r.y - ry,
      n = Math.hypot(dx / Math.max(rx, 1e-9), dy / Math.max(ry, 1e-9));
    // Near enough for a touch: the gap along the line from the centre.
    return n < 1e-9 ? Math.min(rx, ry) : Math.hypot(dx, dy) * Math.abs(1 - 1 / n);
  }
  const corners = [
    { x: r.x, y: r.y },
    { x: r.x + r.w, y: r.y },
    { x: r.x + r.w, y: r.y + r.h },
    { x: r.x, y: r.y + r.h },
  ];
  return Math.min(
    ...corners.map((a, i) => lineDistance(p, a, corners[(i + 1) % 4] ?? a)),
  );
}
/** Where a mark's letter badge is drawn, in canvas pixels. */
export function badgeRect(s: MarkShape, label: string, t: Transform): Transform {
  const width = label.length * 10 + 12,
    p = toViewport(labelPosition(s, { x: width / t.width, y: 25 / t.height }), t);
  return { x: p.x, y: p.y - 25, width, height: 24 };
}
/** A normalized shape at its displayed size, so distances are screen pixels. */
export function scaleShape(
  s: MarkShape,
  width: number,
  height: number,
): MarkShape {
  const scale = <P extends Point>(p: P): P => ({
    ...p,
    x: p.x * width,
    y: p.y * height,
  });
  switch (s.kind) {
    case "rect":
    case "ellipse":
      return {
        ...s,
        rect: {
          x: s.rect.x * width,
          y: s.rect.y * height,
          w: s.rect.w * width,
          h: s.rect.h * height,
        },
      };
    case "arrow":
      return { ...s, from: scale(s.from), to: scale(s.to) };
    case "pin":
      return { ...s, at: scale(s.at) };
    case "stroke":
      return { ...s, points: s.points.map(scale) };
  }
}
export function labelPosition(s: MarkShape, size: Point): Point {
  const b = bounds(s);
  return {
    x: clamp(b.x, 0, Math.max(0, 1 - size.x)),
    y: clamp(b.y, size.y, 1),
  };
}
export function moveShape(s: MarkShape, delta: Point): MarkShape {
  const b = bounds(s),
    dx = clamp(delta.x, -b.x, 1 - b.x - b.w),
    dy = clamp(delta.y, -b.y, 1 - b.y - b.h);
  const move = (p: StrokePoint): StrokePoint => ({
    ...p,
    x: p.x + dx,
    y: p.y + dy,
  });
  switch (s.kind) {
    case "rect":
    case "ellipse":
      return { ...s, rect: { ...s.rect, x: s.rect.x + dx, y: s.rect.y + dy } };
    case "arrow":
      return { ...s, from: move(s.from), to: move(s.to) };
    case "pin":
      return { ...s, at: move(s.at) };
    case "stroke":
      return { ...s, points: s.points.map(move) };
  }
}
export type Tool = MarkShape["kind"];
export const SHAPE_WORD: Record<Tool, string> = {
  stroke: "Pen",
  rect: "Box",
  ellipse: "Ellipse",
  arrow: "Arrow",
  pin: "Pin",
};
export function makeShape(tool: Tool, points: StrokePoint[]): MarkShape {
  const a = points[0] ?? { x: 0.4, y: 0.4, t: null },
    b = points.at(-1) ?? a;
  switch (tool) {
    case "stroke":
      return { kind: "stroke", points };
    case "rect":
    case "ellipse":
      return { kind: tool, rect: rectBetween(a, b) };
    case "arrow":
      return { kind: "arrow", from: a, to: b };
    case "pin":
      return { kind: "pin", at: b };
  }
}
