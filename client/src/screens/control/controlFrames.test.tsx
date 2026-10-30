// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Store, useStore } from "../../state/store";
import type { Picture } from "./useMedia";

vi.mock("./ownership", async (original) => {
  const actual = await original<typeof import("./ownership")>();
  return { ...actual, ownership: vi.fn(actual.ownership) };
});
// Pictures arrive as fast as the computer sends them.
const pictures = new Store<Picture>({ at: null, revision: null, error: null, connected: false });
vi.mock("./useMedia", async (original) => ({
  ...await original<typeof import("./useMedia")>(),
  useMedia: () => ({ picture: useStore(pictures, (p) => p), latest: { current: pictures.get() }, canvas: { current: null }, retry: vi.fn(), reopen: vi.fn() }),
}));

import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Control from "./Control";
import { ownership } from "./ownership";

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

it("redraws only the live picture as its frames arrive", async () => {
  const route = { screen: "control" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "control", depth: 0, active: true, route }}><Control route={route} /></RouteProvider></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(300); });
  expect(screen.getAllByRole("button", { name: /^Watch / }).length).toBeGreaterThan(0);
  vi.mocked(ownership).mockClear();
  for (let i = 1; i <= 12; i++) act(() => pictures.set({ at: Date.now() + i * 83, revision: 1, error: null, connected: true }));
  expect(ownership).not.toHaveBeenCalled();
});
