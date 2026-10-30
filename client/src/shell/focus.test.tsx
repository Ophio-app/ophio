// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { HostSession } from "../host/session";
import { runningTasks } from "../state/projection";
import { Shell } from "./Shell";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
  render(<AppProvider app={app}><Shell /></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(500); });
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const flush = () => act(async () => { await vi.advanceTimersByTimeAsync(50); });
const nav = () => document.querySelector("nav.nav");
const rail = () => document.querySelector(".app__safety .safety-rail");
const bar = () => within(document.querySelector<HTMLElement>(".app__main")!);

/** As on a computer nobody is driving: displays locked and unconfirmed, and
 * no task working on one. Tasks that use no display keep running. */
async function idleDisplays() {
  const host = window.__sim!.host;
  for (const task of host.tasks.filter((t) => t.target_id && runningTasks(app.session.state.get()).some((r) => r.task_id === t.task_id))) {
    host.emit({ type: "task_upserted", task: { ...task, state: "completed", current_run: null } });
  }
  for (const view of host.control) {
    host.emit({ type: "control_changed", view: { ...view, input_state: "unknown", desktop_lock: "locked", lease: null, model_observation: null } });
  }
  await flush();
}

it("a focused screen closes with an X, without the tabs, and Back returns to where it opened", async () => {
  act(() => app.ui.selectTab("work"));
  expect(nav()).not.toBeNull();
  act(() => app.ui.push({ screen: "settings" }));
  await flush();
  expect(nav()).toBeNull();
  expect(document.querySelector(".app")!.classList.contains("app--focused")).toBe(true);
  fireEvent.click(bar().getByRole("button", { name: "Close" }));
  expect(app.ui.route()).toEqual({ screen: "work" });
  expect(nav()).not.toBeNull();
});

it("one focused screen opened from another goes Back, not Close", async () => {
  act(() => app.ui.selectTab("work"));
  act(() => app.ui.push({ screen: "settings" }));
  act(() => app.ui.push({ screen: "accounts" }));
  await flush();
  const bars = [...document.querySelectorAll<HTMLElement>(".bar")].filter((b) => b.closest("[inert], [hidden]") === null);
  const top = within(bars.at(-1)!);
  expect(top.queryByRole("button", { name: "Close" })).toBeNull();
  fireEvent.click(top.getByRole("button", { name: "Back" }));
  expect(app.ui.route()).toEqual({ screen: "settings" });
});

it("a tab's root keeps the tabs and the rail", async () => {
  act(() => app.ui.selectTab("work"));
  await flush();
  expect(nav()).not.toBeNull();
  expect(document.querySelector(".app--focused")).toBeNull();
  expect(rail()).not.toBeNull();
});

it("keeps the whole rail on a focused screen while an agent holds a display", async () => {
  act(() => app.ui.selectTab("work"));
  act(() => app.ui.push({ screen: "settings" }));
  await flush();
  expect(rail()).not.toBeNull();
  expect(document.querySelector(".rail-count")).toBeNull();
});

it("tucks the rail into a count while no one drives a display, and the count opens every Stop", async () => {
  await idleDisplays();
  const running = runningTasks(app.session.state.get());
  expect(running.length).toBeGreaterThan(0);
  act(() => app.ui.selectTab("work"));
  act(() => app.ui.push({ screen: "settings" }));
  await flush();
  expect(rail()).toBeNull();
  const count = screen.getByRole("button", { name: `${running.length} running ${running.length === 1 ? "task" : "tasks"}. Show Stop` });
  expect(count.getAttribute("aria-expanded")).toBe("false");
  fireEvent.click(count);
  await flush();
  expect(rail()!.classList.contains("safety-rail--expanded")).toBe(true);
  for (const task of running) {
    expect(within(rail() as HTMLElement).getByRole("button", { name: new RegExp(`^Stop · ${task.title}`) })).toBeTruthy();
  }
  // Back folds the rail away again before it closes the screen.
  act(() => { window.systemBack!(); });
  expect(rail()).toBeNull();
  expect(app.ui.route()).toEqual({ screen: "settings" });
});

it("with nothing running, a locked or unconfirmed display stays one tap from its controls", async () => {
  await idleDisplays();
  const host = window.__sim!.host;
  for (const task of host.tasks.filter((t) => runningTasks(app.session.state.get()).some((r) => r.task_id === t.task_id))) {
    host.emit({ type: "task_upserted", task: { ...task, state: "completed", current_run: null } });
  }
  await flush();
  expect(runningTasks(app.session.state.get())).toHaveLength(0);
  act(() => app.ui.selectTab("work"));
  act(() => app.ui.push({ screen: "settings" }));
  await flush();
  expect(rail()).toBeNull();
  const safety = screen.getByRole("button", { name: /^Safety · .+ · Computer locked.*\. Show input controls$/ });
  fireEvent.click(safety);
  await flush();
  expect(rail()!.classList.contains("safety-rail--expanded")).toBe(true);
  expect(within(rail() as HTMLElement).getAllByRole("button", { name: /^Pause/ }).length).toBeGreaterThan(0);
});
