// SPDX-License-Identifier: Apache-2.0
//
// Keeps a conversation on its newest line while the person reads there, and
// offers Latest once they have scrolled away. Only the person takes the view
// away. The browser scrolls too, when the keyboard closes or the region or its
// content changes size, sometimes in the frame a new line arrives and before
// the view has followed it; that scroll is the layout moving, not the person
// leaving. A scroll the person is making counts even while lines arrive, so
// reading back during a run is never pulled down. So does a scroll further
// than the layout could have moved the view, such as a screen reader revealing
// an earlier line. Moving focus into the conversation with the keyboard holds
// the view as scrolling does, so new lines do not carry the focused control
// away, until a finger takes over. Latest's own smooth scroll passes through
// positions far from the end on its way down, and does not count as leaving
// either.

import { useLayoutEffect, useRef, useState } from "react";
import { motionReduced } from "../../app/prefs";

/** Within this many pixels of the end, the view is at the newest line. */
const NEAR = 72;
/** How long after the person's last touch, wheel or key a scroll is theirs;
    a fling keeps going after the finger lifts. */
const HELD_MS = 1000;
/** Keys that move focus on through a group, as arrows do through options. */
const STEPS = new Set(["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End"]);
/** Inputs that are one of a group, whose arrows move focus on rather than a caret or a value. */
const CHOICES = new Set(["radio", "checkbox", "button", "submit", "reset", "image"]);
/** Sizes are whole pixels and scroll positions are not. */
const ROUNDING = 2;
const SCROLL_KEYS = new Set(["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "]);

/** Whether an input is the person moving the view, rather than a tap on
    something inside it or typing into a field there. */
function moves(e: Event, el: HTMLElement): boolean {
  if (e.type === "keydown") {
    const t = e.target as HTMLElement | null;
    return SCROLL_KEYS.has((e as KeyboardEvent).key) && !t?.closest?.("input, textarea, select, [contenteditable]");
  }
  // A press on the region itself is its scrollbar; a press on a line is a tap
  // until the finger moves, which touchmove reports.
  if (e.type === "pointerdown") return e.target === el;
  return true;
}
const INPUTS = ["touchmove", "wheel", "keydown", "pointerdown"] as const;

/** Whether keys pressed on an element move a caret or change a value there, rather than focus. */
function edits(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  if (el?.tagName === "INPUT") return !CHOICES.has((el as HTMLInputElement).type);
  return !!el?.closest?.("textarea, select, [contenteditable]");
}

/** The region's and window's heights, and how far the view is from the end. */
function measure(el: HTMLElement) {
  return {
    height: el.scrollHeight,
    view: el.clientHeight,
    win: el.ownerDocument.defaultView?.innerHeight ?? 0,
    gap: Math.max(0, el.scrollHeight - el.scrollTop - el.clientHeight),
  };
}

/**
 * Keep the newest line in view unless the person has scrolled up to read.
 * With no task (the new-task page) the body reads from the top instead.
 */
export function useStickToBottom(key: string | null) {
  const ref = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const jumping = useRef(false);
  /** When the person last moved the view themselves. */
  const heldAt = useRef(-Infinity);
  /** Whether the keyboard moved focus into the region, holding the view. */
  const keyed = useRef(false);
  /** How far from the end the view was when a finger took over, until its tap ends. */
  const took = useRef<number | null>(null);
  /** The region as it was when last seen, to tell how far the layout moved it. */
  const seen = useRef({ height: 0, view: 0, win: 0, gap: 0 });
  const [away, setAway] = useState(false);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setAway(false);
    jumping.current = false;
    heldAt.current = -Infinity;
    keyed.current = false;
    took.current = null;
    if (key === null) {
      pinned.current = false;
      el.scrollTop = 0;
      return;
    }
    pinned.current = true;
    el.scrollTop = el.scrollHeight;
    const doc = el.ownerDocument;
    seen.current = measure(el);
    const held = () => performance.now() - heldAt.current < HELD_MS;
    // Another screen hides the conversation, and a hidden region has no size,
    // which would read as being at the end. It is where it was when last seen.
    const look = () => (el.clientHeight === 0 ? seen.current : measure(el));
    const note = () => {
      seen.current = look();
    };
    const follow = () => {
      if (pinned.current) el.scrollTop = el.scrollHeight;
      note();
    };
    const onScroll = () => {
      const now = look();
      const near = now.gap < NEAR;
      if (jumping.current) {
        if (!near) return;
        jumping.current = false;
      } else if (held()) {
        heldAt.current = performance.now();
      } else if (pinned.current) {
        // The layout moves the view from where it was: by the content that
        // arrived below it, give or take a line, since content can pass through
        // other sizes within a frame; and by the height the region lost. A
        // region that grows only brings the end closer, except that the browser
        // can first give it all the height the window gained and settle the
        // view at that end, before the app's own bars come back into part of
        // it. A scroll that goes further than all that is revealing an earlier
        // line, and stands. One that goes less far than a line past new content
        // stays among the newest lines, which the next line would follow anyway.
        const was = seen.current;
        const grew = now.height - was.height;
        const region = now.view - was.view;
        const left = region > 0 ? Math.max(0, now.win - was.win - region, was.gap - region) : was.gap - region;
        const reach = left + Math.max(0, grew) + (grew > 0 ? NEAR : 0) + ROUNDING;
        if ((grew !== 0 || region !== 0) && now.gap < reach) {
          follow();
          return;
        }
      }
      seen.current = now;
      pinned.current = near && !kept();
      setAway(!pinned.current);
    };
    // The person moving the view takes over from Latest's scroll too.
    const onInput = (e: Event) => {
      if (!moves(e, el)) return;
      heldAt.current = performance.now();
      jumping.current = false;
      took.current = null;
    };
    // Focus the keyboard moves into the region is the person too: the scroll
    // that reveals it is theirs, and new lines leave it where it is until a
    // finger takes over, focus leaves, or they choose Latest. The hold is the
    // focus's alone, and goes with it: the layout scrolls after it are the
    // layout's. Tab moves focus in, and keys move it on inside, as arrows do
    // through a group of options, even where a finger put it; a tap focuses
    // what it lands on, and typing, or moving the caret in a field, moves no
    // focus at all. Released among the newest lines, the view follows them
    // again.
    //
    // A finger that comes down in the region takes over whether or not it
    // moves focus. The browser finds what a tap lands on only as it ends, so
    // nothing moves under the finger until then, even if the browser brings the
    // view to the end meanwhile; a finger that scrolls instead leaves the view
    // to its scroll. A press that never becomes a tap, such as a long one,
    // keeps the view until the next press or key, or until sending or Latest
    // takes the view to the end.
    /** When Tab, which moves focus in, was last pressed. */
    let tabAt = -Infinity;
    /** When Tab or a step through a group, which move focus on inside, was the last key pressed. */
    let stepAt = -Infinity;
    let pressed = false;
    /** Whether focus the keyboard moved, or a tap not yet over, keeps the view where it is. */
    const kept = () => took.current !== null || (keyed.current && el.contains(doc.activeElement));
    const onKey = (e: KeyboardEvent) => {
      pressed = false;
      took.current = null;
      const now = performance.now();
      if (e.key === "Tab") tabAt = now;
      stepAt = e.key === "Tab" || (STEPS.has(e.key) && !edits(e.target)) ? now : -Infinity;
    };
    const onPress = (e: Event) => {
      pressed = true;
      tabAt = -Infinity;
      took.current = null;
      if (!keyed.current || !el.contains(e.target as Node | null)) return;
      keyed.current = false;
      took.current = look().gap;
    };
    const release = (gap = look().gap) => {
      keyed.current = false;
      if (gap >= NEAR) return;
      pinned.current = true;
      setAway(false);
      follow();
    };
    const onFocus = (e: FocusEvent) => {
      const within = !pressed && el.contains(e.relatedTarget as Node | null);
      if (keyed.current && within) return;
      if (performance.now() - (within ? stepAt : tabAt) >= HELD_MS) {
        if (keyed.current) release();
        return;
      }
      keyed.current = true;
      jumping.current = false;
      pinned.current = false;
      setAway(true);
    };
    const onBlur = (e: FocusEvent) => {
      if (keyed.current && !el.contains(e.relatedTarget as Node | null)) release();
    };
    // Lines that arrived during the tap would have been followed.
    const tapped = () => {
      if (took.current === null) return;
      const gap = Math.min(took.current, look().gap);
      took.current = null;
      release(gap);
    };
    // Removing the focused line takes focus away without saying so, sometimes
    // as the next line arrives; the view was among the newest lines if it was
    // just before.
    const resized = () =>
      keyed.current && !el.contains(doc.activeElement) ? release(Math.min(seen.current.gap, look().gap)) : follow();
    // A smooth scroll the browser gave up on short of the end ends the jump too.
    const landed = () => {
      if (!jumping.current) return;
      jumping.current = false;
      onScroll();
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    el.addEventListener("scrollend", landed, { passive: true });
    for (const t of INPUTS) el.addEventListener(t, onInput, { passive: true });
    doc.addEventListener("keydown", onKey, { capture: true, passive: true });
    doc.addEventListener("pointerdown", onPress, { capture: true, passive: true });
    doc.addEventListener("click", tapped, { passive: true });
    el.addEventListener("focusin", onFocus);
    el.addEventListener("focusout", onBlur);
    const ro = new ResizeObserver(resized);
    ro.observe(el);
    if (el.firstElementChild) ro.observe(el.firstElementChild);
    return () => {
      el.removeEventListener("scroll", onScroll);
      el.removeEventListener("scrollend", landed);
      for (const t of INPUTS) el.removeEventListener(t, onInput);
      doc.removeEventListener("keydown", onKey, { capture: true });
      doc.removeEventListener("pointerdown", onPress, { capture: true });
      doc.removeEventListener("click", tapped);
      el.removeEventListener("focusin", onFocus);
      el.removeEventListener("focusout", onBlur);
      ro.disconnect();
    };
  }, [key]);

  const jump = () => {
    const el = ref.current;
    if (!el) return;
    pinned.current = true;
    heldAt.current = -Infinity;
    keyed.current = false;
    took.current = null;
    const smooth = !motionReduced();
    jumping.current = smooth;
    // Latest stays until the view arrives: taking its row away now would cut the scroll short.
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "auto" });
    seen.current = measure(el);
  };
  const sent = () => {
    pinned.current = true;
    heldAt.current = -Infinity;
    keyed.current = false;
    took.current = null;
    jumping.current = false;
    setAway(false);
    const el = ref.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    seen.current = measure(el);
  };
  return { ref, away, jump, sent };
}
