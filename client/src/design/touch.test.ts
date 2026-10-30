// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const reduced = vi.hoisted(() => ({ value: false, watchers: new Set<() => void>() }));
vi.mock("../app/prefs", () => ({
  motionReduced: () => reduced.value,
  watchMotion: (listener: () => void) => {
    reduced.watchers.add(listener);
    return () => reduced.watchers.delete(listener);
  },
}));

function reduceMotion(value: boolean): void {
  reduced.value = value;
  for (const l of reduced.watchers) l();
}

import { startTouch } from "./touch";

interface Fake {
  el: Element;
  keyframes: Keyframe[];
  options: KeyframeAnimationOptions;
  rate: number;
  cancelled: boolean;
  onfinish: (() => void) | null;
  oncancel: (() => void) | null;
  finish(): void;
  cancel(): void;
  updatePlaybackRate(r: number): void;
}

let made: Fake[] = [];
let stop: () => void = () => {};
const realAnimate = Element.prototype.animate;

beforeEach(() => {
  made = [];
  reduced.value = false;
  Element.prototype.animate = function (this: Element, keyframes, options) {
    const a: Fake = {
      el: this,
      keyframes: keyframes as Keyframe[],
      options: options as KeyframeAnimationOptions,
      rate: 1,
      cancelled: false,
      onfinish: null,
      oncancel: null,
      finish() { this.onfinish?.(); },
      cancel() { this.cancelled = true; this.oncancel?.(); },
      updatePlaybackRate(r) { this.rate = r; },
    };
    made.push(a);
    return a as unknown as Animation;
  };
  stop = startTouch();
});

afterEach(() => {
  stop();
  Element.prototype.animate = realAnimate;
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

function mount(html: string): HTMLElement {
  const root = document.createElement("div");
  root.innerHTML = html;
  document.body.append(root);
  for (const el of root.querySelectorAll<HTMLElement>("*")) {
    el.getBoundingClientRect = () => ({ left: 10, top: 20, width: 100, height: 40, right: 110, bottom: 60, x: 10, y: 20, toJSON() {} }) as DOMRect;
  }
  return root;
}

function press(target: Element, init: { x?: number; y?: number; button?: number; isPrimary?: boolean } = {}): void {
  const e = new MouseEvent("pointerdown", { bubbles: true, clientX: init.x ?? 40, clientY: init.y ?? 30, button: init.button ?? 0 });
  Object.defineProperty(e, "isPrimary", { value: init.isPrimary ?? true });
  target.dispatchEvent(e);
}

it("grows from the touch point inside the control, on its own ::after", () => {
  const root = mount(`<ul><li><a class="row--action" href="#x"><span class="label">Prototype</span></a></li></ul>`);
  const host = root.querySelector<HTMLElement>(".row--action")!;
  press(root.querySelector(".label")!, { x: 40, y: 30 });
  expect(host.dataset.reach).toBe("1");
  expect(host.style.getPropertyValue("--reach-x")).toBe("30px");
  expect(host.style.getPropertyValue("--reach-y")).toBe("10px");
  expect(made).toHaveLength(1);
  const grow = made[0]!;
  expect(grow.el).toBe(host);
  expect(grow.options).toMatchObject({ duration: 700, pseudoElement: "::after" });
  // Only compositor properties move, and the origin rides in the transform.
  for (const k of grow.keyframes) for (const p of Object.keys(k)) expect(["transform", "opacity", "offset", "easing"]).toContain(p);
  expect(grow.keyframes[0]!.transform).toBe("translate(30px, 10px) scale(0.1) translate(-30px, -10px)");
});

it("keeps the point inside the control and clears everything when it finishes", () => {
  const root = mount(`<button class="btn">Go</button>`);
  const host = root.querySelector<HTMLElement>(".btn")!;
  press(host, { x: 500, y: -50 });
  expect(host.style.getPropertyValue("--reach-x")).toBe("100px");
  expect(host.style.getPropertyValue("--reach-y")).toBe("0px");
  made[0]!.finish();
  expect(host.dataset.reach).toBeUndefined();
  expect(host.style.getPropertyValue("--reach-x")).toBe("");
  expect(host.style.getPropertyValue("--reach-y")).toBe("");
});

it("cycles its three networks, and a new press replaces the last one cleanly", () => {
  const root = mount(`<button class="chip">A</button>`);
  const host = root.querySelector<HTMLElement>(".chip")!;
  const seen: string[] = [];
  for (let i = 0; i < 4; i++) {
    press(host);
    seen.push(host.dataset.reach!);
  }
  expect(seen).toEqual(["1", "2", "3", "1"]);
  expect(made.slice(0, 3).every((a) => a.cancelled)).toBe(true);
  // The cancelled ones must not strip the running one.
  expect(host.dataset.reach).toBe("1");
  made[3]!.finish();
  expect(host.dataset.reach).toBeUndefined();
});

it("grows a navigation destination inside its pill", () => {
  const root = mount(`<nav><button class="nav__tab"><span class="nav__pill"><svg></svg></span><span>Work</span></button></nav>`);
  press(root.querySelector(".nav__tab span:last-child")!);
  expect(root.querySelector<HTMLElement>(".nav__pill")!.dataset.reach).toBe("1");
  expect(root.querySelector<HTMLElement>(".nav__tab")!.dataset.reach).toBeUndefined();
});

it.each([
  ["Stop", `<button class="btn btn--stop">Stop</button>`],
  ["Stop in the chat header", `<div class="named-stop chat-head__stop"><button class="iconbtn iconbtn--stop">Stop</button></div>`],
  ["an approval", `<div class="approval"><button class="btn btn--primary">Allow</button></div>`],
  ["a review footer", `<div class="review-foot"><button class="btn btn--primary">Send</button></div>`],
  ["an opted-out control", `<button class="btn" data-still>Go</button>`],
  ["a disabled control", `<button class="btn" disabled>Go</button>`],
  ["an unavailable control", `<button class="btn" aria-disabled="true">Go</button>`],
])("leaves %s still", (_, html) => {
  const root = mount(html);
  const button = root.querySelector("button")!;
  press(button);
  button.click();
  expect(root.querySelector("[data-reach]")).toBeNull();
  expect(made).toHaveLength(0);
  expect(document.querySelector(".release")).toBeNull();
});

it("does nothing when motion is reduced", () => {
  reduced.value = true;
  const root = mount(`<button class="btn btn--primary">Pair</button>`);
  const button = root.querySelector("button")!;
  press(button);
  button.click();
  expect(button.dataset.reach).toBeUndefined();
  expect(made).toHaveLength(0);
  expect(document.querySelector(".release")).toBeNull();
});

it("ignores secondary pointers, other buttons and elements that aren't controls", () => {
  const root = mount(`<button class="btn">Go</button><p class="copy">Words</p>`);
  press(root.querySelector(".btn")!, { isPrimary: false });
  press(root.querySelector(".btn")!, { button: 2 });
  press(root.querySelector(".copy")!);
  expect(made).toHaveLength(0);
});

it("withdraws quickly when a touch turns into a scroll", () => {
  const root = mount(`<a class="row--action" href="#x">Row</a>`);
  press(root.querySelector(".row--action")!);
  document.dispatchEvent(new Event("pointercancel"));
  expect(made[0]!.rate).toBe(4);
});

it("lets go of one reused, hidden cloud from a primary action", () => {
  const root = mount(`<button class="btn btn--primary">Pair</button><button class="btn">Later</button>`);
  const primary = root.querySelector<HTMLElement>(".btn--primary")!;
  const plain = root.querySelector<HTMLElement>(".btn:not(.btn--primary)")!;
  plain.click();
  expect(document.querySelector(".release")).toBeNull();

  primary.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 1, clientX: 44, clientY: 33 }));
  const cloud = document.querySelector<HTMLElement>(".release")!;
  expect(cloud.getAttribute("aria-hidden")).toBe("true");
  const flight = made.at(-1)!;
  expect(flight.el).toBe(cloud);
  expect(flight.keyframes[0]!.transform).toContain("translate(44px, 33px)");
  for (const k of flight.keyframes) for (const p of Object.keys(k)) expect(["transform", "opacity", "offset", "easing"]).toContain(p);

  // Keyboard activation has no pointer; the cloud leaves from the button's top.
  primary.click();
  expect(document.querySelectorAll(".release")).toHaveLength(1);
  expect(flight.cancelled).toBe(true);
  expect(made.at(-1)!.keyframes[0]!.transform).toContain("translate(60px, 20px)");
});

it("rises above open sheets through the top layer where it can", () => {
  const show = vi.fn();
  const hide = vi.fn();
  Object.assign(HTMLElement.prototype, { showPopover: show, hidePopover: hide });
  try {
    const root = mount(`<button class="iconbtn iconbtn--primary">Send</button>`);
    root.querySelector("button")!.click();
    const cloud = document.querySelector<HTMLElement>(".release")!;
    expect(cloud.popover).toBe("manual");
    expect(show).toHaveBeenCalledTimes(1);
    made.at(-1)!.finish();
    expect(hide).toHaveBeenCalled();
  } finally {
    delete (HTMLElement.prototype as Partial<HTMLElement>).showPopover;
    delete (HTMLElement.prototype as Partial<HTMLElement>).hidePopover;
  }
});

it("clears a control when the engine can't animate its pseudo-element", () => {
  Element.prototype.animate = () => { throw new TypeError("pseudoElement"); };
  const root = mount(`<button class="btn">Go</button>`);
  const host = root.querySelector<HTMLElement>(".btn")!;
  expect(() => press(host)).not.toThrow();
  expect(host.dataset.reach).toBeUndefined();
  expect(host.style.getPropertyValue("--reach-x")).toBe("");
});

it("stops listening and removes its cloud", () => {
  const root = mount(`<button class="btn btn--primary">Pair</button>`);
  const button = root.querySelector("button")!;
  button.click();
  expect(document.querySelector(".release")).not.toBeNull();
  stop();
  stop = () => {};
  expect(document.querySelector(".release")).toBeNull();
  const count = made.length;
  press(button);
  button.click();
  expect(made).toHaveLength(count);
  expect(button.dataset.reach).toBeUndefined();
});

it("stops a press and a cloud already moving when motion becomes reduced", () => {
  const root = mount(`<button class="btn btn--primary">Pair</button><button class="chip">A</button>`);
  const primary = root.querySelector<HTMLElement>(".btn--primary")!;
  const chip = root.querySelector<HTMLElement>(".chip")!;
  press(chip);
  primary.click();
  const [grow, flight] = made;
  reduceMotion(false);
  expect(grow!.cancelled).toBe(false);
  expect(flight!.cancelled).toBe(false);

  reduceMotion(true);
  expect(grow!.cancelled).toBe(true);
  expect(flight!.cancelled).toBe(true);
  expect(chip.dataset.reach).toBeUndefined();
  expect(chip.style.getPropertyValue("--reach-x")).toBe("");
});

it("leaves no press behind when it stops mid-growth", () => {
  const root = mount(`<button class="chip">A</button><button class="btn">B</button>`);
  const chip = root.querySelector<HTMLElement>(".chip")!;
  const plain = root.querySelector<HTMLElement>(".btn")!;
  press(chip);
  press(plain);
  stop();
  stop = () => {};
  expect(made.every((a) => a.cancelled)).toBe(true);
  for (const host of [chip, plain]) {
    expect(host.dataset.reach).toBeUndefined();
    expect(host.style.getPropertyValue("--reach-y")).toBe("");
  }
  expect(reduced.watchers.size).toBe(0);
});
