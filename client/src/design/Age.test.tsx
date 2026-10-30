// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { Age, relativeAge } from "./Age";
it.each([
  [-240000, "4 min ago", "4 minutes ago"], [-30000, "30 s ago", "30 seconds ago"],
  [-1000, "1 s ago", "1 second ago"], [-999, "now", "just now"], [0, "now", "just now"],
  [999, "now", "just now"], [1000, "in 1 s", "in 1 second"], [1999, "in 1 s", "in 1 second"],
  [2000, "in 2 s", "in 2 seconds"], [30000, "in 30 s", "in 30 seconds"],
  [59999, "in 59 s", "in 59 seconds"], [60000, "in 1 min", "in 1 minute"], [240000, "in 4 min", "in 4 minutes"],
  [3600000, "in 1 h", "in 1 hour"], [86400000, "in 1 d", "in 1 day"],
])("formats deadline offset %i", (offset, text, spoken) => {
  expect(relativeAge(1000000 + offset, 1000000, " ago")).toEqual({ text, spoken });
});
it("uses the caller's present label only within a second", () => {
  expect(relativeAge(1000, 1000, " ago", "just received").text).toBe("just received");
  expect(relativeAge(2000, 1000, " ago", "just received").text).toBe("in 1 s");
});

it("keeps no ticker while the app is hidden and shows the current age as soon as it is shown", () => {
  vi.useFakeTimers({ now: 100_000 });
  const show = (state: "hidden" | "visible") => act(() => {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: state });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  try {
    render(<Age at={100_000} suffix=" ago" />);
    act(() => { vi.advanceTimersByTime(1000); });
    expect(screen.getByText("1 s ago")).toBeTruthy();
    expect(vi.getTimerCount()).toBe(1);
    show("hidden");
    expect(vi.getTimerCount()).toBe(0);
    act(() => { vi.advanceTimersByTime(65_000); });
    show("visible");
    expect(screen.getByText("1 min ago")).toBeTruthy();
    expect(vi.getTimerCount()).toBe(1);
    act(() => { vi.advanceTimersByTime(60_000); });
    expect(screen.getByText("2 min ago")).toBeTruthy();
  } finally {
    cleanup();
    delete (document as { visibilityState?: unknown }).visibilityState;
    vi.useRealTimers();
  }
});
