// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { Toasts } from "../../design/Toast";
import { HostSession } from "../../host/session";
import SafetyStrip from "./SafetyStrip";
import { STOP_WAIT_MS, stopStore } from "./stops";

let app: App;
const tick = (ms = 1500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  render(<AppProvider app={app}><SafetyStrip /><Toasts /></AppProvider>);
});
afterEach(async () => { await tick(7000); cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
const shown = () => document.querySelector(".toasts")?.textContent ?? "";

it("a Stop from the rail is confirmed on screen after the rail moves on to the next task", async () => {
  window.__sim!.latency(900);
  fireEvent.click(screen.getByRole("button", { name: /^Stop · Check the staging/ }));
  expect(shown()).toBe("Stop requested for “Check the staging dashboard renders the new latency chart”");
  await tick();
  // The run has ended and the rail offers the other task's Stop in its place.
  expect(app.session.state.get().tasks[ids.tDash]?.state).toBe("interrupted");
  expect(screen.getByRole("button", { name: /^Stop · Fix the flaky retry test/ })).toBeTruthy();
  expect(shown()).toBe("Stop requested for “Check the staging dashboard renders the new latency chart”");
});

it("Force stop is confirmed on screen", async () => {
  window.__sim!.host.stopSafety.ignore(ids.tDash);
  fireEvent.click(screen.getByRole("button", { name: /^Stop · Check the staging/ })); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^Stop again · Check the staging/ })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Force stop" }));
  fireEvent.click(screen.getByRole("button", { name: `Force stop on ${app.profile.name}` }));
  expect(shown()).toContain("Force stop requested for “Check the staging dashboard renders the new latency chart”");
  await tick();
  expect(app.session.state.get().tasks[ids.tDash]?.state).toBe("interrupted");
});

it.each(["ask", "plan"] as const)("Stop and Force stop still reach what a finished %s turn may have left running", async (mode) => {
  const host = window.__sim!.host;
  act(() => host.stopSafety.leaveUnconfirmed(ids.tDash, mode));
  host.stopSafety.ignore(ids.tDash, false);
  await tick();
  const run = () => app.session.state.get().runs[ids.rDash]!;
  expect(run().state).toBe("completed");
  // Another task now leads the rail; every running task is listed expanded.
  fireEvent.click(screen.getByRole("button", { name: /^Safety ·.* · 2 running tasks$/ }));
  fireEvent.click(screen.getByRole("button", { name: /^Stop · Check the staging/ })); await tick();
  // Stop reached the run but nothing confirmed an end, so it is still offered.
  expect(run().cleanup_pending).toBe(true);
  expect(screen.queryByRole("button", { name: "Force stop" })).toBeNull();
  await tick(STOP_WAIT_MS);
  expect(screen.getByRole("button", { name: "Force stop" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Stop again · Check the staging/ })); await tick();
  await tick(STOP_WAIT_MS);
  fireEvent.click(screen.getByRole("button", { name: "Force stop" }));
  fireEvent.click(screen.getByRole("button", { name: `Force stop on ${app.profile.name}` }));
  await tick();
  expect(run().cleanup_pending).toBeFalsy();
  expect(run().state).toBe("completed");
  expect(screen.queryByRole("button", { name: /· Check the staging/ })).toBeNull();
});
