// SPDX-License-Identifier: Apache-2.0
//
// A vertical swipe on the terminal's output scrolls its history. xterm reads
// the mouse wheel but not touch, so on a phone the history could not be read
// at all. A swipe towards an end the history is already at stays the page's,
// so what lies below the output can still be reached.

export interface TouchScrollTarget {
  /** Rows above the view, and the most there can be (the latest output). */
  position(): { top: number; latest: number };
  scrollLines(lines: number): void;
  /** Height of one row in CSS pixels. */
  rowHeight(): number;
  /** Let a quick swipe run on and slow down after the finger lifts. */
  glide(): boolean;
}

/** Past this many pixels a touch is a swipe, not a tap. */
const SLOP = 8;
/** Held this long before moving, a touch is selecting text. */
const PRESS_MS = 450;
/** Velocity, in pixels per millisecond, below which a glide stops. */
const REST = 0.05;
/** A gliding swipe keeps this share of its speed each millisecond. */
const FRICTION = 0.996;

export function touchScroll(element: HTMLElement, target: TouchScrollTarget): () => void {
  let start: { x: number; y: number; at: number } | null = null;
  let owned = false;
  let lastY = 0;
  let carry = 0;
  let samples: { at: number; y: number }[] = [];
  let frame = 0;

  /** Scrolls by `px` (positive shows later rows); false once the history ends. */
  const scroll = (px: number): boolean => {
    const row = target.rowHeight();
    if (!(row > 0)) return false;
    carry += px / row;
    const lines = Math.trunc(carry);
    if (!lines) return true;
    carry -= lines;
    const { top, latest } = target.position();
    const next = Math.max(0, Math.min(latest, top + lines));
    if (next !== top) target.scrollLines(next - top);
    return next === top + lines;
  };
  const settle = () => {
    cancelAnimationFrame(frame);
    frame = 0;
  };
  const glide = (velocity: number) => {
    let then = performance.now();
    const step = (now: number) => {
      const elapsed = Math.max(0, now - then);
      then = now;
      if (!scroll(velocity * elapsed)) return settle();
      velocity *= FRICTION ** elapsed;
      frame = Math.abs(velocity) < REST ? 0 : requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
  };

  const down = (e: TouchEvent) => {
    settle();
    const t = e.touches.length === 1 ? e.touches[0] : undefined;
    start = t ? { x: t.clientX, y: t.clientY, at: e.timeStamp } : null;
    owned = false;
  };
  const move = (e: TouchEvent) => {
    const t = e.touches.length === 1 ? e.touches[0] : undefined;
    if (!start || !t) {
      start = null;
      return;
    }
    if (!owned) {
      const dx = t.clientX - start.x, dy = t.clientY - start.y;
      if (Math.abs(dx) < SLOP && Math.abs(dy) < SLOP) return;
      const { top, latest } = target.position();
      // A finger moving down reveals earlier rows.
      owned = Math.abs(dy) > Math.abs(dx) && e.timeStamp - start.at < PRESS_MS && (dy > 0 ? top > 0 : top < latest);
      if (!owned) {
        start = null;
        return;
      }
      lastY = start.y;
      carry = 0;
      samples = [];
    }
    e.preventDefault();
    scroll(lastY - t.clientY);
    lastY = t.clientY;
    samples.push({ at: e.timeStamp, y: t.clientY });
    samples = samples.filter((s) => e.timeStamp - s.at <= 100);
  };
  const up = (e: TouchEvent) => {
    // Measured up to the moment of lifting, so a finger that stopped before it
    // lifted does not throw the history on at its earlier speed.
    const recent = samples.find((s) => e.timeStamp - s.at <= 100);
    if (owned && e.type === "touchend" && target.glide() && recent) {
      const velocity = (recent.y - lastY) / Math.max(1, e.timeStamp - recent.at);
      if (Math.abs(velocity) > 0.3) glide(velocity);
    }
    start = null;
    owned = false;
  };

  element.addEventListener("touchstart", down, { passive: true });
  // Not passive: a swipe the history takes must not also scroll the page.
  element.addEventListener("touchmove", move, { passive: false });
  element.addEventListener("touchend", up);
  element.addEventListener("touchcancel", up);
  return () => {
    settle();
    element.removeEventListener("touchstart", down);
    element.removeEventListener("touchmove", move);
    element.removeEventListener("touchend", up);
    element.removeEventListener("touchcancel", up);
  };
}
