// SPDX-License-Identifier: Apache-2.0
import type { CrashKind, CrashRecord } from "../protocol/types";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { crashes } from "../diagnostics/crashes";
import { reports } from "../diagnostics/reports";
import { captureOwnView, onScreenshot } from "../platform/reportNative";
import { Notice } from "../design/Notice";
import { ReportsOverlay, crashTitle } from "./ReportsOverlay";
import { CHIP_MS, PAUSE_MS } from "./prompts";
import { openReport, reportRequest } from "./request";
let screenshot: (() => void) | undefined;
vi.mock("../platform/reportNative", () => ({ takeNativeCrashes: vi.fn(async () => ({ crashes: [] })), captureOwnView: vi.fn(async () => null), screenshotSupported: async () => true, onScreenshot: vi.fn((cb: () => void) => { screenshot = cb; return () => { screenshot = undefined; }; }) }));
const advance = (ms = 0) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(() => {
  vi.useFakeTimers(); localStorage.clear(); crashes.acknowledge(crashes.pending()); reportRequest.set(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(() => { cleanup(); reportRequest.set(null); vi.useRealTimers(); vi.clearAllMocks(); });
it("opens from an error notice with its code and captures before the sheet opens", async () => {
  const capture = vi.mocked(captureOwnView).mockImplementation(async () => { expect(screen.queryByRole("dialog", { name: "Report a problem" })).toBeNull(); return null; });
  render(<><Notice tone="warn" code="unavailable">That action couldn’t finish.</Notice><ReportsOverlay /></>);
  fireEvent.click(screen.getByRole("button", { name: "Report" }));
  await advance(100);
  expect(screen.getByRole("dialog", { name: "Report a problem" })).toBeTruthy();
  expect(reportRequest.get()?.context.error?.code).toBe("unavailable"); expect(capture).toHaveBeenCalledOnce();
});
it("Not now clears these crashes for good, including after a remount", async () => {
  crashes.capture(new Error("Fixture"));
  const { unmount } = render(<ReportsOverlay />); await advance();
  expect(screen.getByText("Send what happened?")).toBeTruthy();
  // A script error left Ophio running.
  expect(screen.getByRole("dialog", { name: "Ophio ran into a problem" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Not now" }));
  expect(crashes.pending()).toEqual([]); unmount();
  render(<ReportsOverlay />); await advance();
  expect(screen.queryByText("Send what happened?")).toBeNull();
});
it("says Ophio closed only when it stopped", async () => {
  render(<ReportsOverlay computerCrashes={[{ at: Date.now(), side: "computer", kind: "panic", code: "panic:runtime.rs:1", build: "test", frames: [] }]} />); await advance();
  expect(screen.getByRole("dialog", { name: /^Ophio on your computer stopped unexpectedly at / })).toBeTruthy();
});
it("says when the computer stopped, and keeps the phone's own stop as it was", () => {
  const now = new Date(2026, 10, 6, 9, 0).getTime();
  const crash = (side: "phone" | "computer", at: Date, kind: CrashKind = "panic"): CrashRecord => ({ at: at.getTime(), side, kind, code: "panic:unknown:1166", build: "test", frames: [] });
  const clock = (d: Date) => d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  // From a problem report: a computer panic half an hour before the phone opened.
  const morning = new Date(2026, 10, 6, 8, 45);
  expect(crashTitle([crash("computer", morning)], now)).toBe(`Ophio on your computer stopped unexpectedly at ${clock(morning)}`);
  const lastNight = new Date(2026, 10, 5, 23, 10);
  expect(crashTitle([crash("computer", lastNight)], now)).toBe(`Ophio on your computer stopped unexpectedly yesterday at ${clock(lastNight)}`);
  const earlier = new Date(2026, 9, 24, 2, 30);
  expect(crashTitle([crash("computer", earlier)], now)).toBe(`Ophio on your computer stopped unexpectedly on ${earlier.toLocaleDateString(undefined, { month: "short", day: "numeric" })} at ${clock(earlier)}`);
  // The newest of several computer stops.
  expect(crashTitle([crash("computer", earlier), crash("computer", morning)], now)).toContain(`at ${clock(morning)}`);
  // The phone's own stop, alone or with the computer's.
  expect(crashTitle([crash("phone", morning, "system")], now)).toBe("Ophio closed unexpectedly");
  expect(crashTitle([crash("computer", morning), crash("phone", morning)], now)).toBe("Ophio closed unexpectedly");
  expect(crashTitle([crash("phone", morning, "script")], now)).toBe("Ophio ran into a problem");
});
it("keeps crashes in the draft through cancelling and sending", async () => {
  crashes.capture(new Error("Fixture"));
  render(<ReportsOverlay />); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Send" })); await advance(100);
  expect(crashes.pending()).toHaveLength(1);
  fireEvent.click(screen.getByRole("button", { name: "Send" })); await advance();
  expect(screen.getByText(/Got it, report/)).toBeTruthy(); expect(crashes.pending()).toEqual([]);
  expect(reports.get().queued.at(-1)?.bundle.crashes).toHaveLength(1);
});
it("offers one screenshot chip and keeps offering after it fades by itself", async () => {
  render(<ReportsOverlay />); await advance(); expect(onScreenshot).toHaveBeenCalledOnce();
  for (let i = 0; i < 5; i++) {
    act(() => { screenshot?.(); screenshot?.(); });
    expect(screen.getAllByRole("button", { name: "Report" })).toHaveLength(1);
    await advance(CHIP_MS);
    expect(screen.queryByRole("button", { name: "Report" })).toBeNull();
  }
  act(() => screenshot?.()); fireEvent.click(screen.getByRole("button", { name: "Report" })); await advance(100);
  expect(reportRequest.get()?.origin).toEqual({ type: "screenshot" });
});
it("pauses the screenshot offer for a day after it is closed three times in a row", async () => {
  render(<ReportsOverlay />); await advance();
  for (let i = 0; i < 3; i++) {
    act(() => screenshot?.());
    fireEvent.click(screen.getByRole("button", { name: "Dismiss report offer" }));
  }
  act(() => screenshot?.()); expect(screen.queryByRole("button", { name: "Report" })).toBeNull();
  await advance(PAUSE_MS);
  act(() => screenshot?.()); expect(screen.getByRole("button", { name: "Report" })).toBeTruthy();
});
it("uses the same sheet for Settings", async () => {
  render(<ReportsOverlay />); act(() => openReport()); await advance(100);
  expect(screen.getByRole("dialog", { name: "Report a problem" })).toBeTruthy();
});
