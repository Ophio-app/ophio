// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { App } from "../app/context";
import { UiStore, type SendAttempt } from "../app/ui";
import { HostSession } from "../host/session";
import { settle } from "../screens/talk/send";
import { announce, LiveRegions, withdraw } from "./Announcer";
import { toast, Toasts } from "./Toast";

/** What each live region holds, polite then assertive. */
const regions = () => [...document.querySelectorAll("[aria-live]")].map((r) => r.textContent);
beforeEach(() => {
  vi.useFakeTimers();
  render(<><LiveRegions /><Toasts /></>);
});
afterEach(async () => {
  await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
  act(() => announce(""));
  cleanup(); vi.useRealTimers();
});

it("holds only the current message: a new one replaces the last in either region", () => {
  act(() => announce("The connection dropped before studio answered.", "assertive"));
  expect(regions()).toEqual(["", "The connection dropped before studio answered."]);
  act(() => announce("Task started in Tide"));
  expect(regions()).toEqual(["Task started in Tide", ""]);
  // Taking back an older message leaves the current one.
  act(() => withdraw("The connection dropped before studio answered."));
  expect(regions()).toEqual(["Task started in Tide", ""]);
  act(() => withdraw("Task started in Tide"));
  expect(regions()).toEqual(["", ""]);
});

it("clears a toast's message when the toast goes", async () => {
  act(() => toast("Saved"));
  expect(regions()).toEqual(["Saved", ""]);
  await act(async () => { await vi.advanceTimersByTimeAsync(3_500); });
  expect(regions()).toEqual(["", ""]);
});

it("clears a send's 'may have arrived' once the computer's answer settles it", () => {
  const link = { connect() {}, disconnect() {}, send: () => false, listen: () => () => {} };
  const app = { ui: new UiStore("announcer"), session: new HostSession("announcer", link), profile: { name: "studio" } } as unknown as App;
  const attempt: SendAttempt = { phase: "unknown", at: 1, commandId: "sent", text: "Check the chart",
    detail: "The connection dropped before studio answered. It may have arrived.",
    body: { command: "send_message", task_id: "task", text: "Check the chart", bundle: null, delivery: "next_turn" } };
  app.ui.saveDraft("project", "task", { text: "Check the chart", attempt });
  act(() => announce(attempt.detail, "assertive"));
  act(() => settle(app, "project", "task", attempt, { command_id: "sent", status: "rejected", seq: null, result: null, error: null }));
  expect(regions()).toEqual(["", ""]);
  expect(app.ui.draft("project", "task")?.attempt?.phase).toBe("rejected");
});

// A phone screen reader walking the page stops on whatever the regions still
// hold, an old message with nothing on screen, first on every screen.
it("lets a message go once it has had time to be read, but never a newer one early", async () => {
  act(() => announce("Denied. The agent carries on without it."));
  await act(async () => { await vi.advanceTimersByTimeAsync(6_000); });
  expect(regions()).toEqual(["Denied. The agent carries on without it.", ""]);
  act(() => announce("Task started in Tide", "assertive"));
  await act(async () => { await vi.advanceTimersByTimeAsync(6_000); });
  expect(regions()).toEqual(["", "Task started in Tide"]);
  await act(async () => { await vi.advanceTimersByTimeAsync(4_000); });
  expect(regions()).toEqual(["", ""]);
});
