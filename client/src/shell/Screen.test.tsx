// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore, type Route } from "../app/ui";
import { HostSession } from "../host/session";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { RouteView } from "../screens/routes";
import { RouteProvider, Screen } from "./Screen";

let app: App;

beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await act(async () => { await vi.advanceTimersByTimeAsync(900); });
});
afterEach(() => {
  cleanup();
  app.session.stop();
  window.__sim?.host.dispose();
  vi.clearAllTimers();
  vi.useRealTimers();
});

it("only the screen on show lifts toasts above its pinned footer", () => {
  const route: Route = { screen: "receipt", taskId: ids.tDash };
  const draw = (active: boolean, footer = true) => <AppProvider app={app}>
    <RouteProvider value={{ tab: "work", route, depth: 1, active }}>
      <Screen title="Receipt" footer={footer ? <button type="button">Send</button> : undefined}>Body</Screen>
    </RouteProvider>
  </AppProvider>;
  const footerHeight = () => document.documentElement.style.getPropertyValue("--screen-footer-h");
  const view = render(draw(false));
  expect(footerHeight()).toBe("");
  view.rerender(draw(true));
  expect(footerHeight()).toMatch(/^\d+px$/);
  view.rerender(draw(true, false));
  expect(footerHeight()).toBe("");
  view.rerender(draw(true));
  view.unmount();
  expect(footerHeight()).toBe("");
});

it("a root's header takes no taps until its screen has loaded", async () => {
  const route: Route = { screen: "work" };
  const view = render(<AppProvider app={app}>
    <RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><RouteView route={route} /></RouteProvider>
  </AppProvider>);
  // The picker in the placeholder would open a sheet that closes when the
  // real screen replaces it.
  expect(view.container.querySelector(".screen__loading")).not.toBeNull();
  expect(view.container.querySelector("header.ctx")?.hasAttribute("inert")).toBe(true);
  await act(async () => { await vi.dynamicImportSettled(); await vi.advanceTimersByTimeAsync(50); });
  expect(view.container.querySelector(".screen__loading")).toBeNull();
  expect(view.container.querySelector("header.ctx")?.hasAttribute("inert")).toBe(false);
});

it("a loading screen shows its outline, not a blank page", async () => {
  const route: Route = { screen: "settings" };
  const view = render(<AppProvider app={app}>
    <RouteProvider value={{ tab: "work", route, depth: 1, active: true }}><RouteView route={route} /></RouteProvider>
  </AppProvider>);
  // A pushed screen keeps its way out while it loads.
  expect(view.getByRole("button", { name: "Close" })).toBeTruthy();
  const outline = view.container.querySelector(".screen__loading .skeleton");
  expect(outline?.getAttribute("aria-hidden")).toBe("true");
  expect(outline?.querySelectorAll(".skeleton__row")).toHaveLength(3);
  expect(view.getByText("Loading…").className).toBe("visually-hidden");
  await act(async () => { await vi.dynamicImportSettled(); await vi.advanceTimersByTimeAsync(50); });
  expect(view.container.querySelector(".skeleton")).toBeNull();
});
