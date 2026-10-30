// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { touchScroll, type TouchScrollTarget } from "./touchScroll";

/** A history of `latest` rows above the view, 20px a row, starting at `top`. */
function history(top: number, latest: number, glide = true) {
  const at = { top, latest };
  const moved: number[] = [];
  const target: TouchScrollTarget = {
    position: () => ({ ...at }),
    scrollLines: (lines) => { moved.push(lines); at.top += lines; },
    rowHeight: () => 20,
    glide: () => glide,
  };
  const element = document.createElement("div");
  document.body.append(element);
  const dispose = touchScroll(element, target);
  let now = 0;
  const send = (type: string, touches: { x: number; y: number }[], after = 16) => {
    now += after;
    const e = new Event(type, { bubbles: true, cancelable: true });
    Object.defineProperties(e, {
      touches: { value: touches.map(t => ({ clientX: t.x, clientY: t.y })) },
      timeStamp: { value: now },
    });
    element.dispatchEvent(e);
    return e;
  };
  /** A finger from `from` to `to` in `steps` moves, then lifted; whether the page was kept from scrolling. */
  const swipe = (from: number, to: number, { steps = 6, x = 0, wait = 16, lift = "touchend" } = {}) => {
    send("touchstart", [{ x: 100, y: from }]);
    let held = false;
    for (let i = 1; i <= steps; i++)
      held = send("touchmove", [{ x: 100 + x * i / steps, y: from + (to - from) * i / steps }], i === 1 ? wait : 16).defaultPrevented || held;
    send(lift, []);
    return held;
  };
  return { at, moved, element, dispose, send, swipe };
}

const frames: FrameRequestCallback[] = [];
beforeEach(() => {
  frames.length = 0;
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => frames.push(cb));
  vi.stubGlobal("cancelAnimationFrame", (id: number) => { if (id) delete frames[id - 1]; });
});
afterEach(() => { vi.unstubAllGlobals(); document.body.replaceChildren(); });
/** Runs every pending frame 16 ms apart until none is left. */
const settle = () => {
  let t = performance.now();
  for (let i = 0; i < frames.length; i++) frames[i]?.((t += 16));
};

describe("touchScroll", () => {
  it("scrolls the history with a vertical swipe and keeps the page still", () => {
    const h = history(100, 100, false);
    expect(h.swipe(300, 420)).toBe(true);
    // A finger moving down 120px reveals six earlier rows.
    expect(h.moved.reduce((a, b) => a + b, 0)).toBe(-6);
    expect(h.at.top).toBe(94);
    expect(h.swipe(420, 340, { steps: 4 })).toBe(true);
    expect(h.at.top).toBe(98);
  });

  it("counts part rows across moves rather than dropping them", () => {
    const h = history(100, 100, false);
    // Twelve moves of 5px: no single move is a row, together they are three.
    expect(h.swipe(300, 360, { steps: 12 })).toBe(true);
    expect(h.at.top).toBe(97);
  });

  it("leaves a swipe towards an end the history is at to the page", () => {
    const latest = history(100, 100);
    expect(latest.swipe(400, 300)).toBe(false);
    expect(latest.moved).toEqual([]);
    const oldest = history(0, 100);
    expect(oldest.swipe(300, 400)).toBe(false);
    expect(oldest.moved).toEqual([]);
  });

  it("stops at the oldest row and hands nothing more on", () => {
    const h = history(2, 100, false);
    h.swipe(100, 400);
    expect(h.moved).toEqual([-2]);
    expect(h.at.top).toBe(0);
  });

  it("leaves sideways swipes, long presses and two fingers alone", () => {
    const h = history(50, 100, false);
    expect(h.swipe(300, 320, { x: 120 })).toBe(false);
    // Held before moving, the touch is selecting text.
    expect(h.swipe(300, 400, { wait: 600 })).toBe(false);
    h.send("touchstart", [{ x: 100, y: 300 }, { x: 200, y: 300 }]);
    expect(h.send("touchmove", [{ x: 100, y: 400 }, { x: 200, y: 400 }]).defaultPrevented).toBe(false);
    h.send("touchend", []);
    expect(h.moved).toEqual([]);
  });

  it("lets a quick swipe glide on and slow to a stop", () => {
    const h = history(200, 200);
    h.swipe(300, 480, { steps: 6 });
    const lifted = h.at.top;
    expect(frames.length).toBe(1);
    settle();
    expect(h.at.top).toBeLessThan(lifted);
    expect(frames.length).toBeGreaterThan(2);
    // It came to rest on its own, before the history ran out.
    expect(h.at.top).toBeGreaterThan(0);
  });

  it("does not glide when the finger came to rest before it lifted", () => {
    const h = history(200, 200);
    h.send("touchstart", [{ x: 100, y: 300 }]);
    for (let i = 1; i <= 6; i++) h.send("touchmove", [{ x: 100, y: 300 + 30 * i }]);
    const lifted = h.at.top;
    // Half a second held still, then lifted.
    h.send("touchend", [], 500);
    expect(frames).toEqual([]);
    expect(h.at.top).toBe(lifted);
  });

  it("does not glide under reduced motion, after a cancelled touch, or once a finger lands again", () => {
    const still = history(200, 200, false);
    still.swipe(300, 480);
    expect(frames).toEqual([]);
    const cancelled = history(200, 200);
    cancelled.swipe(300, 480, { lift: "touchcancel" });
    expect(frames).toEqual([]);
    const caught = history(200, 200);
    caught.swipe(300, 480);
    const lifted = caught.at.top;
    caught.send("touchstart", [{ x: 100, y: 300 }]);
    settle();
    expect(caught.at.top).toBe(lifted);
  });

  it("stops listening once disposed", () => {
    const h = history(100, 100);
    h.dispose();
    expect(h.swipe(300, 420)).toBe(false);
    expect(h.moved).toEqual([]);
  });
});
