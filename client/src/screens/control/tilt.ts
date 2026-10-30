// SPDX-License-Identifier: Apache-2.0
//
// Tilt to pan a zoomed picture, as a marble rolls: tip the phone toward the
// part you want to see. The angle the phone is held at when tilt is switched
// on is level; a few degrees either way do nothing, so a hand's tremor never
// drifts the picture, and the further past that, the faster it moves.

import { useEffect, useRef } from "react";
import type { Point } from "./gestures";

/** Degrees from level that do nothing, and where the speed stops rising. */
export const TILT_DEAD = 4;
export const TILT_FULL = 25;
/** CSS pixels a second at full tilt. */
export const TILT_SPEED = 900;

/** Gravity in the screen's own axes, as a unit vector: x to the right of the
 * picture, y up it. The sensor's axes are the phone's, which turn with it;
 * `angle` is the screen's rotation from the phone's natural orientation. */
export function screenGravity(
  g: { x: number; y: number; z: number },
  angle: number,
): { x: number; y: number; z: number } | null {
  const length = Math.hypot(g.x, g.y, g.z);
  if (!Number.isFinite(length) || length < 1) return null;
  const r = (angle * Math.PI) / 180;
  const cos = Math.cos(r), sin = Math.sin(r);
  return {
    x: (g.x * cos - g.y * sin) / length,
    y: (g.x * sin + g.y * cos) / length,
    z: g.z / length,
  };
}

/** How far to move the picture each second for a tilt from level. */
export function tiltVelocity(level: Point, now: Point): Point {
  const speed = (d: number) => {
    const degrees = (Math.asin(Math.max(-1, Math.min(1, d))) * 180) / Math.PI;
    const past = Math.min(1, Math.max(0, (Math.abs(degrees) - TILT_DEAD) / (TILT_FULL - TILT_DEAD)));
    return Math.sign(degrees) * past * past * TILT_SPEED;
  };
  // Tipping the right edge down shows more of the right; tipping the top
  // edge away shows more of the top.
  return { x: speed(now.x - level.x), y: -speed(now.y - level.y) };
}

/** Calls `pan` each frame with how far the picture should move, while `on`. */
export function useTiltPan(on: boolean, pan: (by: Point) => void): void {
  const latest = useRef(pan);
  latest.current = pan;
  useEffect(() => {
    if (!on || typeof window === "undefined" || !("DeviceMotionEvent" in window)) return;
    let level: Point | null = null;
    let tilt: Point | null = null;
    let frame = 0;
    let last = 0;
    const relevel = () => { level = null; };
    const motion = (e: DeviceMotionEvent) => {
      const a = e.accelerationIncludingGravity;
      if (a?.x == null || a.y == null || a.z == null) return;
      const g = screenGravity({ x: a.x, y: a.y, z: a.z }, screen.orientation?.angle ?? 0);
      if (!g) return;
      tilt = { x: g.x, y: g.y };
      level ??= tilt;
    };
    const step = (at: number) => {
      const dt = last ? Math.min(0.1, (at - last) / 1000) : 0;
      last = at;
      if (level && tilt) {
        const v = tiltVelocity(level, tilt);
        if (v.x || v.y) latest.current({ x: v.x * dt, y: v.y * dt });
      }
      frame = requestAnimationFrame(step);
    };
    window.addEventListener("devicemotion", motion);
    screen.orientation?.addEventListener("change", relevel);
    frame = requestAnimationFrame(step);
    return () => {
      window.removeEventListener("devicemotion", motion);
      screen.orientation?.removeEventListener("change", relevel);
      cancelAnimationFrame(frame);
    };
  }, [on]);
}
