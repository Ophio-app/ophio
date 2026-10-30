// SPDX-License-Identifier: Apache-2.0
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { HostSession } from "../host/session";
import { ColdStartOffline } from "./ColdStartOffline";
import { RouteProvider } from "./Screen";

afterEach(() => cleanup());

it("shows the connection fallback on To-Do before this phone has heard its computer", async () => {
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  const ui = new UiStore(profile.profileId);
  const app: App = { platform, profile, profiles: [profile], session, ui,
    switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  const route = { screen: "todo" } as const;
  expect(session.state.get().host).toBeNull();
  render(<AppProvider app={app}><RouteProvider value={{ tab: "todo", depth: 0, active: true, route }}>
    <ColdStartOffline route={route}><p>Uncached To-Do content</p></ColdStartOffline>
  </RouteProvider></AppProvider>);
  expect(screen.getByRole("heading", { name: `Can’t reach ${profile.name}` })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Retry connection" })).toBeTruthy();
  expect(screen.queryByText("Uncached To-Do content")).toBeNull();
  ui.retire(); session.stop();
});
