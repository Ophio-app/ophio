// SPDX-License-Identifier: Apache-2.0
// Seen in the app: a list's row counts as handled only once the person, using
// the app, has had it well on screen for a moment; an item's own screen
// counts at once.
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createSimPlatform } from "../dev/sim/platform";
import type { Platform } from "../host/link";
import { forgetHandled, SEEN_DWELL_MS, SEEN_RECENT_USE_MS, SEEN_VISIBLE_RATIO, useHandledOnScreen, useHandledWhenSeen } from "./handled";
import { appLock } from "./lock";

/** Stands in for the browser's observer: the test says how much of a row shows. */
class FakeObserver {
  static all = new Set<FakeObserver>();
  readonly targets = new Set<Element>();
  constructor(readonly callback: IntersectionObserverCallback, readonly options?: IntersectionObserverInit) { FakeObserver.all.add(this); }
  observe(el: Element) { this.targets.add(el); }
  unobserve(el: Element) { this.targets.delete(el); }
  disconnect() { this.targets.clear(); FakeObserver.all.delete(this); }
  takeRecords() { return []; }
}
function show(el: Element, ratio: number) {
  for (const o of [...FakeObserver.all]) if (o.targets.has(el))
    act(() => o.callback([{ target: el, isIntersecting: ratio > 0, intersectionRatio: ratio } as IntersectionObserverEntry], o as unknown as IntersectionObserver));
}

let platform: Platform;
let stopLock = () => {};
const advance = async (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
function visibility(value: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { configurable: true, value });
  document.dispatchEvent(new Event("visibilitychange"));
}
/** The ids told as handled, from the notifications taken off this phone. */
const told = () => window.__sim!.device.cancelled.map((tag) => tag.split("/").at(-1));
/** The person touches the app. */
const touch = () => fireEvent.pointerDown(document.body);

beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear(); forgetHandled(); FakeObserver.all.clear();
  vi.stubGlobal("IntersectionObserver", FakeObserver);
  visibility("visible");
  platform = createSimPlatform();
  stopLock = appLock(platform).start();
  await advance(0);
});
afterEach(() => {
  cleanup(); stopLock(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); visibility("visible");
});

function Row({ id }: { id: string }) {
  const ref = useHandledWhenSeen(platform, "p1", [id], true);
  return <div ref={ref} data-testid={id}>{id}</div>;
}
const row = (id: string) => document.querySelector(`[data-testid="${id}"]`)!;

it("asks the observer about half the row", () => {
  render(<Row id="a" />);
  expect([...FakeObserver.all][0]?.options?.threshold).toBe(SEEN_VISIBLE_RATIO);
});

it("isn't seen on screen if nobody has used the app lately", async () => {
  render(<Row id="a" />);
  show(row("a"), 1);
  await advance(SEEN_DWELL_MS * 10);
  expect(told()).toEqual([]);
  // Used long ago doesn't count either.
  cleanup(); touch(); await advance(SEEN_RECENT_USE_MS + 1);
  render(<Row id="b" />);
  show(row("b"), 1); await advance(SEEN_DWELL_MS * 2);
  expect(told()).toEqual([]);
});

it("isn't seen in under two seconds, or less than half on screen", async () => {
  render(<Row id="a" />);
  touch();
  show(row("a"), 1);
  await advance(SEEN_DWELL_MS - 100);
  // Scrolled away just before: the two seconds start over.
  show(row("a"), 0);
  await advance(SEEN_DWELL_MS);
  show(row("a"), SEEN_VISIBLE_RATIO - 0.1);
  await advance(SEEN_DWELL_MS * 2);
  expect(told()).toEqual([]);
});

it("is seen once after two seconds at least half on screen while the app is used", async () => {
  render(<Row id="a" />);
  touch();
  show(row("a"), SEEN_VISIBLE_RATIO);
  await advance(SEEN_DWELL_MS);
  expect(told()).toEqual(["a"]);
  // Once per session, however often it shows.
  show(row("a"), 0); show(row("a"), 1); touch();
  await advance(SEEN_DWELL_MS * 2);
  cleanup(); render(<Row id="a" />); show(row("a"), 1); touch();
  await advance(SEEN_DWELL_MS * 2);
  expect(told()).toEqual(["a"]);
});

it("counts once the person uses the app, if the row has been on screen long enough", async () => {
  render(<Row id="a" />);
  show(row("a"), 1);
  await advance(SEEN_DWELL_MS * 3);
  expect(told()).toEqual([]);
  fireEvent.keyDown(document.body, { key: "ArrowDown" }); await advance(0);
  expect(told()).toEqual(["a"]);
});

it("a row in a collapsed group is never seen, until the group opens", async () => {
  render(<details data-testid="group"><summary>2 requests</summary><Row id="a" /></details>);
  touch();
  show(row("a"), 1);
  await advance(SEEN_DWELL_MS * 3);
  touch(); await advance(SEEN_DWELL_MS * 3);
  expect(told()).toEqual([]);
  const group = row("group") as HTMLDetailsElement;
  act(() => { group.open = true; group.dispatchEvent(new Event("toggle")); });
  touch(); await advance(SEEN_DWELL_MS);
  expect(told()).toEqual(["a"]);
});

it("going to the background stops the two seconds, and they start over on return", async () => {
  render(<Row id="a" />);
  touch();
  show(row("a"), 1);
  await advance(SEEN_DWELL_MS - 500);
  act(() => visibility("hidden"));
  await advance(SEEN_DWELL_MS * 2);
  expect(told()).toEqual([]);
  act(() => visibility("visible"));
  show(row("a"), 1); touch();
  await advance(SEEN_DWELL_MS - 500);
  expect(told()).toEqual([]);
  await advance(500);
  expect(told()).toEqual(["a"]);
});

it("the item's own screen counts at once, with nobody touching it", async () => {
  function Own() { useHandledOnScreen(platform, "p1", ["own"], true); return null; }
  render(<Own />);
  await advance(0);
  expect(told()).toEqual(["own"]);
});

it("nothing in a list is seen where the observer is missing", async () => {
  vi.stubGlobal("IntersectionObserver", undefined);
  render(<Row id="a" />);
  touch(); await advance(SEEN_DWELL_MS * 2);
  expect(told()).toEqual([]);
});
