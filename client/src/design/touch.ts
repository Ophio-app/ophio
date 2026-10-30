// SPDX-License-Identifier: Apache-2.0
//
// How ordinary controls answer a finger. A press grows a fine network out from
// the touch point behind the control's label (material.css draws it on the
// control's own ::after, so it scrolls and leaves with the control), and a
// primary action lets go of a small cloud of specks. Only transform and
// opacity move, on the compositor: no React state, no timers, nothing loops.
// Consent and emergency controls stay still, as does everything when motion
// is reduced, including whatever was already moving when it became so.

import { motionReduced, watchMotion } from "../app/prefs";

const HOSTS = ".btn, .iconbtn, .row--action, .chip, .segmented__opt, .evidence--action, .ctx__context, .status, .chat-head__back, .nav__tab";
const STILL = ".btn--stop, .btn--stop-outline, .btn--warn, .iconbtn--stop, .chat-head__stop, .approval, .review-foot, .safety-rail, .app__safety, [data-still]";
const RELEASES = ".btn--primary, .iconbtn--primary";
const OFF = ":disabled, [aria-disabled='true']";

const GROW = "cubic-bezier(0.2, 0.8, 0.2, 1)";

let turn = 0;
let last: HTMLElement | null = null;
const growing = new Map<HTMLElement, Animation>();
let cloud: HTMLElement | null = null;
let drift: Animation | null = null;

function still(el: Element): boolean {
  return el.closest(STILL) !== null || el.matches(OFF);
}

function clear(host: HTMLElement): void {
  delete host.dataset.reach;
  host.style.removeProperty("--reach-x");
  host.style.removeProperty("--reach-y");
}

/** Grow a network from (clientX, clientY) inside the host, then let it fade. */
function spread(host: HTMLElement, clientX: number, clientY: number): void {
  const box = host.getBoundingClientRect();
  if (!box.width || !box.height) return;
  const x = Math.round(Math.min(Math.max(clientX - box.left, 0), box.width));
  const y = Math.round(Math.min(Math.max(clientY - box.top, 0), box.height));
  const before = growing.get(host);
  growing.delete(host);
  before?.cancel();

  turn = (turn % 3) + 1;
  host.style.setProperty("--reach-x", `${x}px`);
  host.style.setProperty("--reach-y", `${y}px`);
  host.dataset.reach = String(turn);
  // The origin is baked into the transform: animating transform-origin would
  // take the animation off the compositor.
  const at = (s: number) => `translate(${x}px, ${y}px) scale(${s}) translate(${-x}px, ${-y}px)`;
  let grow: Animation;
  try {
    grow = host.animate(
      [
        { transform: at(0.1), opacity: 0, easing: GROW },
        { opacity: 1, offset: 0.18, easing: "cubic-bezier(0.4, 0, 0.6, 1)" },
        { transform: at(1), opacity: 0 },
      ],
      { duration: 700, pseudoElement: "::after" },
    );
  } catch {
    clear(host);
    return;
  }
  growing.set(host, grow);
  last = host;
  const done = () => {
    if (growing.get(host) !== grow) return;
    growing.delete(host);
    if (last === host) last = null;
    clear(host);
  };
  grow.onfinish = done;
  grow.oncancel = done;
}

/** One reused element in the top layer, so it also rises over open sheets. */
function specks(): HTMLElement {
  if (!cloud) {
    cloud = document.createElement("div");
    cloud.className = "release";
    cloud.setAttribute("aria-hidden", "true");
    if ("showPopover" in cloud) cloud.popover = "manual";
  }
  if (!cloud.isConnected) document.body.append(cloud);
  if (cloud.popover) {
    // Showing it again puts it above a sheet opened since the last time.
    settle(cloud);
    cloud.showPopover();
  }
  return cloud;
}

function settle(el: HTMLElement): void {
  try {
    if (el.popover) el.hidePopover();
  } catch {
    // Already hidden, or taken out of the document.
  }
}

function release(host: HTMLElement, e: MouseEvent): void {
  const box = host.getBoundingClientRect();
  const pointed = e.detail > 0 && (e.clientX !== 0 || e.clientY !== 0);
  const x = Math.round(pointed ? e.clientX : box.left + box.width / 2);
  const y = Math.round(pointed ? e.clientY : box.top);
  drift?.cancel();
  const el = specks();
  const tilt = turn % 2 ? 8 : -8;
  const rise = 52 + turn * 4;
  let flight: Animation;
  try {
    flight = el.animate(
      [
        { transform: `translate(${x}px, ${y}px) scale(0.4) rotate(0deg)`, opacity: 0, easing: "cubic-bezier(0.2, 0.7, 0.3, 1)" },
        { opacity: 1, offset: 0.16, easing: "ease-in" },
        { transform: `translate(${x}px, ${y - rise}px) scale(1.35) rotate(${tilt}deg)`, opacity: 0 },
      ],
      { duration: 950 },
    );
  } catch {
    return;
  }
  drift = flight;
  flight.onfinish = () => {
    if (drift !== flight) return;
    drift = null;
    settle(el);
  };
}

function press(e: PointerEvent): void {
  if (!e.isPrimary || e.button !== 0 || motionReduced()) return;
  const found = e.target instanceof Element ? e.target.closest(HOSTS) : null;
  if (!(found instanceof HTMLElement) || still(found)) return;
  const host = found.matches(".nav__tab") ? found.querySelector<HTMLElement>(".nav__pill") : found;
  if (host) spread(host, e.clientX, e.clientY);
}

/** A touch that turned into a scroll withdraws its network quickly. */
function abandon(): void {
  if (last) growing.get(last)?.updatePlaybackRate(4);
  last = null;
}

function clicked(e: MouseEvent): void {
  if (motionReduced()) return;
  const found = e.target instanceof Element ? e.target.closest(RELEASES) : null;
  if (found instanceof HTMLElement && !still(found)) release(found, e);
}

/** Stop everything in motion at once and leave no trace on any control. */
function halt(): void {
  // Cleared here rather than by oncancel, which may run later.
  const running = [...growing];
  growing.clear();
  last = null;
  for (const [host, grow] of running) {
    grow.cancel();
    clear(host);
  }
  const flight = drift;
  drift = null;
  flight?.cancel();
  if (cloud) settle(cloud);
}

function motionChanged(): void {
  if (motionReduced()) halt();
}

/** Listen once for the whole document; returns a function that stops. */
export function startTouch(doc: Document = document): () => void {
  const opts = { capture: true, passive: true } as const;
  doc.addEventListener("pointerdown", press, opts);
  doc.addEventListener("pointercancel", abandon, opts);
  doc.addEventListener("click", clicked, opts);
  const unwatch = watchMotion(motionChanged);
  return () => {
    doc.removeEventListener("pointerdown", press, opts);
    doc.removeEventListener("pointercancel", abandon, opts);
    doc.removeEventListener("click", clicked, opts);
    unwatch();
    halt();
    cloud?.remove();
    cloud = null;
    turn = 0;
  };
}
