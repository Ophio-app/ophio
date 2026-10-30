// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore, type Route } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { HostSession } from "../host/session";
import { ids } from "../dev/sim/scenario";
import { openAttention } from "../state/projection";
import { RouteView } from "../screens/routes";
import { RouteProvider } from "./Screen";
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
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const shown = async () => {
  app.session.start(); await vi.advanceTimersByTimeAsync(900);
  render(<AppProvider app={app}><Shell /></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(500); });
};

it("starts at Home, so a new conversation isn't in a project by accident", async () => {
  await shown();
  expect(app.ui.get().projectId).toBe(ids.pHome);
});

it("keeps the project the person chose", async () => {
  app.ui.selectProject(ids.pNotes);
  await shown();
  expect(app.ui.get().projectId).toBe(ids.pNotes);
});

it("leaves the choice to the shell, so Chat can't pick the busiest project first", async () => {
  // Chat once chose the project of the latest task as it mounted, and when it
  // was on screen before the computer answered, its choice came first.
  app.session.start(); await vi.advanceTimersByTimeAsync(900);
  const route: Route = { screen: "talk" };
  render(<AppProvider app={app}>
    <RouteProvider value={{ tab: "talk", route, depth: 0, active: true }}><RouteView route={route} /></RouteProvider>
  </AppProvider>);
  await act(async () => { await vi.dynamicImportSettled(); await vi.advanceTimersByTimeAsync(500); });
  expect(document.querySelector(".screen__loading")).toBeNull();
  expect(app.ui.get().projectId).toBeNull();
});

it("a first run opens on Tasks when something there already needs the person", async () => {
  await shown();
  expect(openAttention(app.session.state.get()).length).toBeGreaterThan(0);
  expect(app.ui.get().tab).toBe("work");
  expect(document.querySelector(".nav [aria-current='page']")?.textContent).toContain("Tasks");
});

it("a first run with nothing waiting stays on Chat, and never moves once it has been asked", async () => {
  const host = window.__sim!.host;
  host.tasks = []; host.runs = []; host.attention = []; host.approvals = [];
  await shown();
  expect(openAttention(app.session.state.get()).map((a) => a.kind)).toEqual([]);
  expect(app.ui.get().tab).toBe("talk");
  // Something needing them later is Tasks's badge, not a jump.
  app.ui.openFirst(true);
  expect(app.ui.get().tab).toBe("talk");
});

it("a returning person opens where they left, even with something waiting", async () => {
  app.ui.selectTab("talk");
  app.ui.selectTask(ids.pTide, null);
  expect(app.ui.persistNow()).toBe(true);
  app = { ...app, ui: new UiStore(app.profile.profileId) };
  await shown();
  expect(openAttention(app.session.state.get()).length).toBeGreaterThan(0);
  expect(app.ui.get().tab).toBe("talk");
});

it("a first run leaves alone someone who has already gone somewhere", async () => {
  app.ui.selectTab("library");
  await shown();
  expect(app.ui.get().tab).toBe("library");
});
