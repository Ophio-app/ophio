// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PhoneTrail } from "./trail";
import { MAX_TRAIL_EVENTS, PHONE_TRAIL_MS } from "./constants";

beforeEach(() => { localStorage.clear(); vi.useFakeTimers(); vi.setSystemTime(1_760_000_000_000); });
afterEach(() => vi.useRealTimers());
it("evicts by count and by age, including when no new action arrives", () => {
  const trail = new PhoneTrail();
  for (let i = 0; i < MAX_TRAIL_EVENTS + 5; i++) trail.record({ type: "command", command: "start_task", outcome: "accepted", ms: i });
  expect(trail.snapshot()).toHaveLength(MAX_TRAIL_EVENTS);
  expect(trail.snapshot()[0]?.event).toMatchObject({ ms: 5 });
  vi.advanceTimersByTime(PHONE_TRAIL_MS + 1);
  expect(trail.snapshot()).toEqual([]);
});
it("throttles saves and reloads only the recent tail", () => {
  const storage = { getItem: vi.fn(() => localStorage.getItem("test")), setItem: vi.fn((_k: string, value: string) => localStorage.setItem("test", value)) };
  const trail = new PhoneTrail(Date.now, storage);
  trail.record({ type: "route", route: "/work" });
  trail.record({ type: "notice", key: "expired", tone: "error" });
  expect(storage.setItem).not.toHaveBeenCalled();
  vi.advanceTimersByTime(1000);
  expect(storage.setItem).toHaveBeenCalledTimes(1);
  expect(new PhoneTrail(Date.now, storage).snapshot()).toEqual(trail.snapshot());
  vi.setSystemTime(Date.now() + PHONE_TRAIL_MS + 1);
  expect(new PhoneTrail(Date.now, storage).snapshot()).toEqual([]);
});
it("flushes on pagehide and when hidden", () => {
  const trail = new PhoneTrail();
  const off = trail.followLifecycle();
  trail.record({ type: "route", route: "/work" });
  window.dispatchEvent(new Event("pagehide"));
  expect(new PhoneTrail().snapshot()).toEqual(trail.snapshot());
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
  document.dispatchEvent(new Event("visibilitychange"));
  expect(new PhoneTrail().snapshot().at(-1)?.event).toEqual({ type: "lifecycle", state: "background" });
  off(); vi.restoreAllMocks();
});
it("never throws when storage or the clock fails, and preserves damaged saved JSON", () => {
  const bad = { getItem() { throw new Error("storage unavailable"); }, setItem() { throw new Error("full"); } };
  const trail = new PhoneTrail(() => { throw new Error("clock"); }, bad);
  expect(() => trail.record({ type: "route", route: "/work" })).not.toThrow();
  expect(() => trail.flush()).not.toThrow();
  localStorage.setItem("diagnostics.trail.v1", "{broken");
  const recovered = new PhoneTrail();
  recovered.record({ type: "route", route: "/work" }); recovered.flush();
  expect(localStorage.getItem("diagnostics.trail.v1.unreadable")).toBe("{broken");
});
