// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { HostSession } from "../host/session";
import { ProjectSheet } from "./ProjectSheet";
import { ContextHeader } from "./ContextHeader";
import { BottomNav } from "./BottomNav";
import { Shell } from "./Shell";
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
  const added = window.__sim!.host.projects.find((p) => p.host_path.endsWith("/weather-station"))!;
  expect(app.ui.get().projectId).toBe(added.project_id);
  expect(close).toHaveBeenCalledTimes(1);
  // Opened again, it is the list once more, not the add sheet.
  rerender(<AppProvider app={app}><ProjectSheet open={false} onClose={close} /></AppProvider>); await advance();
  rerender(<AppProvider app={app}><ProjectSheet open onClose={close} /></AppProvider>); await advance();
  expect(screen.getByRole("button", { name: /^Connections and readiness/ })).toBeTruthy();
});

it("selects a newly added project across tabs even when its host event arrives after the receipt", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  render(<AppProvider app={app}><Shell /></AppProvider>); await advance();
  act(() => app.ui.selectTab("talk"));
  const host = window.__sim!.host;
  const emit = host.emit.bind(host);
  let held: Parameters<typeof host.emit>[0] | undefined;
  vi.spyOn(host, "emit").mockImplementation((event) => {
    if (event.type === "project_upserted" && event.project.host_path.endsWith("/weather-station")) {
      held = event;
      return 0;
    }
    return emit(event);
  });
  fireEvent.click(document.querySelector(".tab:not(.is-hidden) .ctx__context")!); await advance();
  fireEvent.click(screen.getByRole("button", { name: /^Add project/ })); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Open Projects" })); await advance();
  fireEvent.click(screen.getByRole("checkbox", { name: "Select weather-station" }));
  fireEvent.click(screen.getByRole("button", { name: "Review selection (1)" }));
  fireEvent.click(screen.getByRole("button", { name: "Add 1 project" })); await advance();
  const added = host.projects.find((p) => p.host_path.endsWith("/weather-station"))!;
  expect(held).toBeDefined();
  expect(app.session.state.get().projects[added.project_id]).toBeUndefined();
  expect(screen.queryByRole("dialog", { name: "Add project" })).toBeNull();
  act(() => emit(held!)); await advance();
  expect(app.ui.get().projectId).toBe(added.project_id);
  for (const tab of ["work", "library", "todo", "talk"] as const) {
    act(() => app.ui.selectTab(tab)); await advance();
    expect(app.ui.get().projectId).toBe(added.project_id);
  }
});

it("selects a project added by entering its folder path", async () => {
  render(<AppProvider app={app}><ProjectSheet open onClose={close} /></AppProvider>); await advance();
  fireEvent.click(screen.getByRole("button", { name: /^Add project/ })); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Enter a path instead" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Folder on computer" }), { target: { value: "/home/you/code/new-project" } });
  fireEvent.click(screen.getByRole("button", { name: "Add project" })); await advance();
  const added = window.__sim!.host.projects.find((p) => p.host_path === "/home/you/code/new-project")!;
  expect(app.ui.get().projectId).toBe(added.project_id);
  expect(close).toHaveBeenCalledTimes(1);
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

it("Chat All stays on this computer and choosing a Chat project preserves To-Do All", async () => {
  app.all = true;
  act(() => { app.ui.selectProject(ids.pTide); app.ui.setTodoAll(true); });
  render(<AppProvider app={app}><ProjectSheet open allowAll conversations onClose={close} /></AppProvider>); await advance();
  const picker = screen.getByRole("dialog", { name: /^Projects/ });
  expect(picker.textContent).toContain(`On ${app.profile.name}`);
  expect(picker.textContent).not.toContain("On every computer");
  const all = screen.getByRole("button", { name: /^All projects/ });
  expect(all.textContent).toContain("Conversations from every project");
  fireEvent.click(all);
  expect(app.ui.get()).toMatchObject({ projectId: ids.pTide, talkAll: true, todoAll: true });
  expect(all.getAttribute("aria-current")).toBe("true");
  const project = screen.getByRole("button", { name: /^Home/ });
  expect(project.getAttribute("aria-current")).not.toBe("true");
  fireEvent.click(project);
  expect(app.ui.get()).toMatchObject({ projectId: ids.pHome, talkAll: false, todoAll: true });
  expect(project.getAttribute("aria-current")).toBe("true");
});

it("only the conversation list header names Chat All and its current computer", async () => {
  app.all = true;
  act(() => { app.ui.selectProject(ids.pTide); app.ui.setTalkAll(true); });
  const { rerender } = render(<AppProvider app={app}><ContextHeader conversations heading /></AppProvider>); await advance();
  expect(document.querySelector(".ctx__project-name")?.textContent).toBe("All projects");
  expect(document.querySelector(".ctx__computer")?.textContent).toBe(`On ${app.profile.name}`);
  rerender(<AppProvider app={app}><ContextHeader heading /></AppProvider>); await advance();
  expect(document.querySelector(".ctx__project-name")?.textContent).toBe("tidewater-api");
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

it("keeps a project's unread results and what needs the person as two counts, its pills together", async () => {
  render(<AppProvider app={app}><ProjectSheet open onClose={close} /></AppProvider>); await advance();
  const row = screen.getByRole("button", { name: /^tidewater-api/ });
  // Different things: finished or failed results not yet looked at, and questions or approvals waiting.
  expect(row.querySelector(".unread-count")?.textContent).toMatch(/^\d+ unread$/);
  expect(row.textContent).toMatch(/\d+ needs? you/);
  // One group that can take the row's width, so the pills don't stack one per line.
  expect(row.querySelector(".row__pills")?.classList.contains("row__pills--counts")).toBe(true);
});
