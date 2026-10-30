// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { HostSession } from "../host/session";
import { RouteProvider } from "./Screen";
import { ContextHeader } from "./ContextHeader";
import Talk from "../screens/talk/Talk";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const route = { screen: "talk" } as const;
const talk = () => render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", route, depth: 0, active: true }}><Talk route={route} /></RouteProvider></AppProvider>);
const removeEveryProject = () => act(() => {
  const host = window.__sim!.host;
  for (const p of host.projects) host.emit({ type: "project_removed", project_id: p.project_id });
  host.projects = [];
});
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing fixture profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("adds a project from the header's project switcher", async () => {
  const { container } = render(<AppProvider app={app}><ContextHeader /></AppProvider>);
  fireEvent.click(container.querySelector(".ctx__context")!);
  fireEvent.click(screen.getByRole("button", { name: /^Add project/ }));
  await advance();
  fireEvent.click(screen.getByRole("button", { name: "Enter a path instead" }));
  fireEvent.change(screen.getByLabelText("Folder on computer"), { target: { value: "/home/you/Projects/garden" } });
  fireEvent.click(screen.getByRole("button", { name: "Add project" }));
  await advance(900);
  const added = Object.values(app.session.state.get().projects).find((p) => p.host_path === "/home/you/Projects/garden");
  expect(added).toBeTruthy();
  expect(app.ui.get().projectId).toBe(added!.project_id);
});

it("explains an empty project switcher and still offers to add one", async () => {
  removeEveryProject();
  const { container } = render(<AppProvider app={app}><ContextHeader /></AppProvider>);
  fireEvent.click(container.querySelector(".ctx__context")!);
  expect(screen.getByText(/No projects yet/)).toBeTruthy();
  expect(screen.getByRole("button", { name: /^Add project/ })).toBeTruthy();
});

it("checks the path on Talk's first-run form and keeps what was typed", async () => {
  removeEveryProject();
  talk();
  await advance();
  fireEvent.change(screen.getByLabelText("Folder on computer"), { target: { value: "code/garden" } });
  fireEvent.change(screen.getByLabelText("Project name (optional)"), { target: { value: "Garden" } });
  fireEvent.click(screen.getByRole("button", { name: "Add project" }));
  await advance();
  expect(screen.getByText(/Use an absolute path/)).toBeTruthy();
  expect(Object.values(app.session.commands.get()).some((c) => c.body.command === "add_project")).toBe(false);
  cleanup();
  talk();
  await advance();
  expect(screen.getByLabelText<HTMLInputElement>("Folder on computer").value).toBe("code/garden");
  expect(screen.getByLabelText<HTMLInputElement>("Project name (optional)").value).toBe("Garden");
  fireEvent.change(screen.getByLabelText("Folder on computer"), { target: { value: "/home/you/Projects/garden" } });
  fireEvent.click(screen.getByRole("button", { name: "Add project" }));
  await advance(900);
  const added = Object.values(app.session.state.get().projects).find((p) => p.name === "Garden");
  expect(added?.host_path).toBe("/home/you/Projects/garden");
  expect(app.ui.get().projectId).toBe(added!.project_id);
  expect(localStorage.getItem(`work.${app.profile.profileId}.project-path`)).toBe('""');
  expect(localStorage.getItem(`work.${app.profile.profileId}.project-name`)).toBe('""');
});


it("keeps the mounted first-run form in sync with edits in the header form", async () => {
  removeEveryProject();
  const { container } = talk();
  fireEvent.change(screen.getByLabelText("Folder on computer"), { target: { value: "/home/you/Projects/garden" } });
  fireEvent.change(screen.getByLabelText("Project name (optional)"), { target: { value: "Garden" } });
  fireEvent.click(container.querySelector(".ctx__context")!);
  fireEvent.click(within(screen.getByRole("dialog", { name: "Projects" })).getByRole("button", { name: /^Add project/ }));
  await advance();
  const sheet = within(screen.getByRole("dialog", { name: "Add project" }));
  fireEvent.click(sheet.getByRole("button", { name: "Enter a path instead" }));
  expect(sheet.getByLabelText<HTMLInputElement>("Folder on computer").value).toBe("/home/you/Projects/garden");
  expect(sheet.getByLabelText<HTMLInputElement>("Project name (optional)").value).toBe("Garden");
  fireEvent.change(sheet.getByLabelText("Folder on computer"), { target: { value: "/home/you/Projects/orchard" } });
  fireEvent.change(sheet.getByLabelText("Project name (optional)"), { target: { value: "Orchard" } });
  fireEvent.click(sheet.getByRole("button", { name: "Close" }));
  await advance();
  expect(screen.getByLabelText<HTMLInputElement>("Folder on computer").value).toBe("/home/you/Projects/orchard");
  expect(screen.getByLabelText<HTMLInputElement>("Project name (optional)").value).toBe("Orchard");
  fireEvent.click(screen.getByRole("button", { name: "Add project" }));
  await advance(900);
  const projects = Object.values(app.session.state.get().projects);
  expect(projects).toHaveLength(1);
  expect(projects[0]).toMatchObject({ name: "Orchard", host_path: "/home/you/Projects/orchard" });
  expect(app.ui.get().projectId).toBe(projects[0]!.project_id);
  expect(localStorage.getItem(`work.${app.profile.profileId}.project-path`)).toBe('""');
  expect(localStorage.getItem(`work.${app.profile.profileId}.project-name`)).toBe('""');
});

it("keeps first-run entry disabled offline and without Manage permission", async () => {
  removeEveryProject();
  talk();
  fireEvent.change(screen.getByLabelText("Folder on computer"), { target: { value: "/home/you/Projects/garden" } });
  window.__sim!.offline(); await advance();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Add project" }).disabled).toBe(true);
  expect(screen.getByText("Offline", { selector: ".notice__title" })).toBeTruthy();
  window.__sim!.online(); await advance(900);
  window.__sim!.permissions({ admin: false }); await advance(900);
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Add project" }).disabled).toBe(true);
  expect(screen.getByText(/Adding projects needs this device’s Manage permission/)).toBeTruthy();
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "add_project")).toBe(false);
});
