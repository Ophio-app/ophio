// SPDX-License-Identifier: Apache-2.0
import type { TargetPoint } from "../../protocol/types";

export interface Point {
  x: number;
  y: number;
}
export interface ViewTransform {
  left: number;
  top: number;
  width: number;
  height: number;
  zoom: number;
  pan: Point;
  dpr: number;
}
export const clamp = (n: number, min = 0, max = 1) =>
  Math.min(max, Math.max(min, n));

/** Pointer coordinates are CSS pixels; backing pixels cancel with DPR. */
export function targetPoint(
  point: Point,
  view: ViewTransform,
): TargetPoint | null {
  if (
    ![
      point.x,
      point.y,
      view.left,
      view.top,
      view.width,
      view.height,
      view.zoom,
      view.dpr,
      view.pan.x,
      view.pan.y,
    ].every(Number.isFinite) ||
    view.width <= 0 ||
    view.height <= 0 ||
    view.zoom <= 0 ||
    view.dpr <= 0
  )
    return null;
  const x =
    ((point.x - view.left - view.pan.x) * view.dpr) /
    (view.width * view.dpr * view.zoom);
  const y =
    ((point.y - view.top - view.pan.y) * view.dpr) /
    (view.height * view.dpr * view.zoom);
  return x < 0 || y < 0 || x > 1 || y > 1 ? null : { x, y };
}

export const TAP_SLOP = 8;
export const LONG_PRESS_MS = 550;
export type Gesture = "tap" | "long_press" | "pan" | "pinch";
export function recognizeGesture(
  start: Point,
  end: Point,
  elapsed: number,
  maxPointers: number,
  distance = 0,
): Gesture {
  if (maxPointers > 1) return "pinch";
  if (
    Math.max(distance, Math.hypot(end.x - start.x, end.y - start.y)) > TAP_SLOP
  )
    return "pan";
  return elapsed >= LONG_PRESS_MS ? "long_press" : "tap";
}

export function pinchView(
  view: { zoom: number; pan: Point },
  center: Point,
  distanceRatio: number,
  delta: Point,
) {
  const zoom = clamp(view.zoom * distanceRatio, 1, 4);
  if (zoom === 1) return { zoom, pan: { x: 0, y: 0 } };
  const ratio = zoom / view.zoom;
  return {
    zoom,
    pan: {
      x: center.x - (center.x - view.pan.x) * ratio + delta.x,
      y: center.y - (center.y - view.pan.y) * ratio + delta.y,
    },
  };
}

/** A zoomed picture never pans past its edges, so the surface stays covered. */
export function clampPan(
  view: { zoom: number; pan: Point },
  size: { width: number; height: number },
) {
  if (view.zoom <= 1) return { zoom: 1, pan: { x: 0, y: 0 } };
  return {
    zoom: view.zoom,
    pan: {
      x: clamp(view.pan.x, size.width * (1 - view.zoom), 0),
      y: clamp(view.pan.y, size.height * (1 - view.zoom), 0),
    },
  };
}

/** Touch: two taps this close in time and place are a double tap. */
export const DOUBLE_TAP_MS = 300;
export const DOUBLE_TAP_SLOP = 48;
/** Touch: a finger that slides this soon after landing may be the first of
 * two, so its drag waits to see whether a second arrives. */
export const DRAG_DELAY_MS = 100;

/** A tap at `at` and `point` that follows `previous` closely enough. */
export function isDoubleTap(
  previous: { at: number; point: Point } | null,
  at: number,
  point: Point,
): boolean {
  return (
    !!previous &&
    at - previous.at <= DOUBLE_TAP_MS &&
    Math.hypot(point.x - previous.point.x, point.y - previous.point.y) <=
      DOUBLE_TAP_SLOP
  );
}

/**
 * Moves a zoomed picture by `delta` as far as its edges allow, as a zoomed
 * page does on Android; `rest` is what the picture could not take, for the
 * computer to scroll. A pan already past an edge is not pulled back.
 */
export function panWithin(
  view: { zoom: number; pan: Point },
  delta: Point,
  box: { width: number; height: number },
): { pan: Point; rest: Point } {
  const axis = (pan: number, d: number, size: number) => {
    const next = clamp(pan + d, Math.min(size * (1 - view.zoom), pan), Math.max(0, pan));
    return [next, d - (next - pan)] as const;
  };
  const [x, restX] = axis(view.pan.x, delta.x, box.width);
  const [y, restY] = axis(view.pan.y, delta.y, box.height);
  return { pan: { x, y }, rest: { x: restX, y: restY } };
}

/** Two fingers either pinch the picture or scroll the computer, decided once
 * per gesture by whichever moves past the slop first: the gap between them,
 * or the point between them. */
export const TWO_FINGER_SLOP = 18;
export function twoFingerIntent(
  startGap: number,
  gap: number,
  startCenter: Point,
  center: Point,
): "zoom" | "scroll" | null {
  const spread = Math.abs(gap - startGap);
  const travel = Math.hypot(center.x - startCenter.x, center.y - startCenter.y);
  if (spread < TWO_FINGER_SLOP && travel < TWO_FINGER_SLOP) return null;
  return spread >= travel ? "zoom" : "scroll";
}

/** A zoomed picture follows the pointer, as a remote desktop's view does: when
 * the pointer nears an edge of the surface, the view moves to keep it in. */
export function followPointer(
  view: { zoom: number; pan: Point },
  at: TargetPoint,
  size: { width: number; height: number },
  margin = 0.15,
) {
  if (view.zoom <= 1) return view;
  const shift = (pan: number, p: number, length: number) => {
    const on = pan + p * length * view.zoom;
    const edge = length * margin;
    return on < edge ? pan + edge - on : on > length - edge ? pan - (on - (length - edge)) : pan;
  };
  return clampPan(
    {
      zoom: view.zoom,
      pan: { x: shift(view.pan.x, at.x, size.width), y: shift(view.pan.y, at.y, size.height) },
    },
    size,
  );
}

/** Steps a region's edges snap to, so small pans ask for the same region. */
const REGION_GRID = 512;

/**
 * The part of the target the zoomed view shows, with a margin of an eighth
 * of the view on each side so short pans stay sharp, in fractions of the
 * target. `null` when the whole target is shown.
 */
export function visibleRegion(
  view: { zoom: number; pan: Point },
  box: { width: number; height: number },
): { x: number; y: number; width: number; height: number } | null {
  if (
    !(view.zoom > 1) ||
    !(box.width > 0) ||
    !(box.height > 0) ||
    ![view.zoom, view.pan.x, view.pan.y, box.width, box.height].every(
      Number.isFinite,
    )
  )
    return null;
  const span = 1 / view.zoom,
    margin = span / 8;
  const edges = (pan: number, size: number) => {
    const start = -pan / (size * view.zoom);
    return [
      clamp(Math.floor((start - margin) * REGION_GRID) / REGION_GRID),
      clamp(Math.ceil((start + span + margin) * REGION_GRID) / REGION_GRID),
    ] as const;
  };
  const [x0, x1] = edges(view.pan.x, box.width);
  const [y0, y1] = edges(view.pan.y, box.height);
  if (x1 <= x0 || y1 <= y0 || (x0 === 0 && y0 === 0 && x1 === 1 && y1 === 1))
    return null;
  return { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
}
