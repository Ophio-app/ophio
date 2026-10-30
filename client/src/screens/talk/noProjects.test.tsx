// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Talk from "./Talk";

let app: App;
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); history.replaceState(null, "", "/"); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });

async function folderExample(host: string) {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  history.replaceState(null, "", `/?host=${host}`);
  const platform = createSimPlatform();
  window.__sim!.host.projects = [];
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: { screen: "talk" } }}><Talk route={{ screen: "talk" }} /></RouteProvider></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(250); });
  return screen.getByLabelText("Folder on computer").getAttribute("placeholder");
}

it("gives the first project's folder example in the form that computer writes one", async () => {
  expect(await folderExample("windows")).toBe("C:\\Users\\you\\code\\project");
});

it("gives a Linux computer a Linux folder example", async () => {
  expect(await folderExample("hyprland")).toBe("/home/you/code/project");
});
