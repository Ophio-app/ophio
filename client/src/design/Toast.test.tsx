// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { LiveRegions } from "./Announcer";
import { toast, Toasts } from "./Toast";

const TEXT = "The connection dropped before the computer answered. Your draft is kept in Review.";
beforeEach(() => {
  vi.useFakeTimers();
  render(<><LiveRegions /><Toasts /></>);
});
afterEach(async () => {
  await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
  cleanup(); vi.useRealTimers();
});

it("announces the text once and keeps the action and Dismiss reachable", () => {
  const run = vi.fn();
  act(() => toast(TEXT, { tone: "stop", action: { label: "Review", run } }));
  const live = [...document.querySelectorAll("[aria-live]")].filter((r) => r.textContent?.includes(TEXT));
  expect(live).toHaveLength(1);
  const region = screen.getByRole("region", { name: "Notifications" });
  // The toast itself is read on the way through, not announced again.
  expect(region.closest("[aria-live], [aria-hidden=true]")).toBeNull();
  expect(region.querySelector("[aria-live]")).toBeNull();
  expect(region.textContent).toContain(TEXT);
  fireEvent.click(screen.getByRole("button", { name: "Review" }));
  expect(run).toHaveBeenCalledOnce();
  expect(screen.queryByRole("region", { name: "Notifications" })).toBeNull();
});

it("lets every toast be dismissed", () => {
  act(() => toast("Saved"));
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  expect(screen.queryByText("Saved", { selector: ".toast__text" })).toBeNull();
});

it("stays up while focus is inside it and leaves after focus does", async () => {
  act(() => toast(TEXT, { action: { label: "Review", run() {} } }));
  const action = screen.getByRole("button", { name: "Review" });
  act(() => action.focus());
  await act(async () => { await vi.advanceTimersByTimeAsync(20_000); });
  expect(screen.getByRole("button", { name: "Review" })).toBe(action);
  // Moving between its own buttons keeps it up.
  act(() => screen.getByRole("button", { name: "Dismiss" }).focus());
  await act(async () => { await vi.advanceTimersByTimeAsync(20_000); });
  expect(screen.getByRole("button", { name: "Review" })).toBe(action);
  act(() => (document.activeElement as HTMLElement).blur());
  await act(async () => { await vi.advanceTimersByTimeAsync(7_000); });
  expect(screen.queryByRole("button", { name: "Review" })).toBeNull();
});

it("shows at most the newest two", () => {
  act(() => { toast("One"); toast("Two"); toast("Three"); });
  expect([...document.querySelectorAll(".toast__text")].map((p) => p.textContent)).toEqual(["Two", "Three"]);
});

it("tells an undo window it closed unless its action was taken", () => {
  const expire = vi.fn();
  act(() => toast("Removed", { action: { label: "Undo", run() {} }, expire }));
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  expect(expire).not.toHaveBeenCalled();
  act(() => toast("Removed", { action: { label: "Undo", run() {} }, expire }));
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  expect(expire).toHaveBeenCalledOnce();
  act(() => { toast("Removed", { action: { label: "Undo", run() {} }, expire }); toast("Two"); toast("Three"); });
  expect(expire).toHaveBeenCalledTimes(2);
});
