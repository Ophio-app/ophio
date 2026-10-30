// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { HostSession } from "../host/session";
import { ProjectSheet } from "./ProjectSheet";
import { BottomNav } from "./BottomNav";
import LegacyChecklistRedirect from "../screens/LegacyChecklistRedirect";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const close = vi.fn();
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear(); close.mockClear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("adds a project straight from the project list", async () => {
  const before = window.__sim!.host.projects.length;
  expect(before).toBeGreaterThan(0);
  const { rerender } = render(<AppProvider app={app}><ProjectSheet open onClose={close} /></AppProvider>); await advance();
  fireEvent.click(screen.getByRole("button", { name: /^Add project/ })); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Open Projects" })); await advance();
  fireEvent.click(screen.getByRole("checkbox", { name: "Select weather-station" }));
  fireEvent.click(screen.getByRole("button", { name: "Review selection (1)" }));
  fireEvent.click(screen.getByRole("button", { name: "Add 1 project" })); await advance();
  expect(window.__sim!.host.projects).toHaveLength(before + 1);
  expect(close).toHaveBeenCalledTimes(1);
  // Opened again, it is the list once more, not the add sheet.
  rerender(<AppProvider app={app}><ProjectSheet open={false} onClose={close} /></AppProvider>); await advance();
  rerender(<AppProvider app={app}><ProjectSheet open onClose={close} /></AppProvider>); await advance();
  expect(screen.getByRole("button", { name: /^Connections and readiness/ })).toBeTruthy();
});

it("leads with Home, and narrows the projects as the person types without the keyboard coming up first", async () => {
  render(<AppProvider app={app}><ProjectSheet open onClose={close} /></AppProvider>); await advance();
  const rows = () => screen.getAllByRole("button", { name: /^(Home|tidewater-api|field-notes|garden-planner|Garden zine)/ }).map((b) => b.textContent!);
  expect(rows()[0]).toMatch(/^Home/);
  const search = screen.getByRole("searchbox", { name: "Search projects" });
  // Only a tap into it brings the keyboard up.
  expect(document.activeElement).not.toBe(search);
  fireEvent.change(search, { target: { value: "TIDE" } });
  expect(rows()).toEqual([expect.stringMatching(/^tidewater-api/)]);
  expect(screen.queryByRole("button", { name: /^Add project/ })).toBeNull();
  fireEvent.change(search, { target: { value: "notes" } });
  expect(rows()).toEqual([expect.stringMatching(/^field-notes/)]);
  fireEvent.change(search, { target: { value: "nothing like it" } });
  expect(screen.getByText("No project matches “nothing like it”.")).toBeTruthy();
  fireEvent.change(search, { target: { value: "home" } });
  fireEvent.click(screen.getByRole("button", { name: /^Home/ }));
  expect(app.ui.get().projectId).toBe(ids.pHome);
  expect(close).toHaveBeenCalledTimes(1);
});

it("tells a computer from before Home to update, and keeps its projects", async () => {
  const host = window.__sim!.host;
  const home = host.projects.find((p) => p.home)!;
  act(() => { host.projects = host.projects.filter((p) => !p.home); host.emit({ type: "project_removed", project_id: home.project_id }); });
  render(<AppProvider app={app}><ProjectSheet open onClose={close} /></AppProvider>); await advance();
  // Shown, but not a choice.
  expect(screen.queryByRole("button", { name: /^Home/ })).toBeNull();
  expect(screen.getByText(/Update Øphio on .+ to work in its home folder/)).toBeTruthy();
  expect(screen.getByRole("button", { name: /^tidewater-api/ })).toBeTruthy();
});

it("shows All only on To-Do and choosing a project exits All", async () => {
  act(() => app.ui.selectProject(ids.pTide));
  render(<AppProvider app={app}><ProjectSheet open allowAll onClose={close} /></AppProvider>); await advance();
  fireEvent.click(screen.getByRole("button", { name: /^All projects/ }));
  expect(app.ui.get()).toMatchObject({ projectId: ids.pTide, todoAll: true });
  fireEvent.click(screen.getByRole("button", { name: /^Home/ }));
  expect(app.ui.get()).toMatchObject({ projectId: ids.pHome, todoAll: false });
});

it("shows five ordered tabs, with To-Do after Library", async () => {
  const { container } = render(<AppProvider app={app}><BottomNav /></AppProvider>);
  await advance(); await advance();
  const labels = [...container.querySelectorAll(".nav__label")].map((n) => n.textContent);
  expect(labels).toEqual(["Chat", "Control", "Tasks", "Library", "To-Do"]);
  fireEvent.click(screen.getByRole("button", { name: /^To-Do/ }));
  expect(app.ui.get().tab).toBe("todo");
});

it("redirects a legacy checklist route rendered directly to To-Do", async () => {
  act(() => { app.ui.selectTab("talk"); app.ui.setTodoAll(true); });
  const route = { screen: "checklist", projectId: ids.pTide, itemId: "stripe-key" } as const;
  render(<AppProvider app={app}><LegacyChecklistRedirect route={route} /></AppProvider>);
  await advance(); await advance();
  expect(app.ui.get()).toMatchObject({ tab: "todo", projectId: ids.pTide, todoItemId: "stripe-key", todoAll: false });
});
