// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

vi.mock("./brief", async (original) => {
  const actual = await original<typeof import("./brief")>();
  return { ...actual, buildBrief: vi.fn(actual.buildBrief) };
});

import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import { ColdStartOffline } from "../../shell/ColdStartOffline";
import { RouteProvider } from "../../shell/Screen";
import { buildBrief } from "./brief";
import Work from "./Work";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const hear = (at: number) => act(() => app.session.state.set((s) => ({ ...s, lastHeardAt: at })));

it("leaves Tasks as it is while only the time last heard moves", async () => {
  const route = { screen: "work" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 0, active: true, route }}><Work route={route} /></RouteProvider></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  expect(screen.getAllByRole("heading").length).toBeGreaterThan(0);
  vi.mocked(buildBrief).mockClear();
  for (const at of [1_000_001, 1_000_002, 1_000_003]) hear(at);
  expect(buildBrief).not.toHaveBeenCalled();
});

it("records when the computer was last reached once per change, however many screens are mounted", () => {
  const setItem = vi.spyOn(Storage.prototype, "setItem");
  const route = { screen: "talk" as const };
  render(<AppProvider app={app}>{[0, 1, 2].map((i) => <ColdStartOffline key={i} route={route}><p>Screen {i}</p></ColdStartOffline>)}</AppProvider>);
  expect(screen.getByText("Screen 2")).toBeTruthy();
  setItem.mockClear();
  hear(2_000_001);
  hear(2_000_002);
  const key = `last-reached.${app.profile.profileId}`;
  expect(setItem.mock.calls.filter(([k]) => k === key)).toEqual([[key, "2000001"], [key, "2000002"]]);
  expect(localStorage.getItem(key)).toBe("2000002");
});
