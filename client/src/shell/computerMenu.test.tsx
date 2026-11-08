// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
vi.mock("./freshness", async (original) => {
  const actual = await original<typeof import("./freshness")>();
  return { ...actual, freshness: vi.fn(actual.freshness) };
});

import { AppProvider, type App } from "../app/context";
import { freshness } from "./freshness";
import { Computers } from "../app/computers";
import { UiStore } from "../app/ui";
import { prefs } from "../app/prefs";
import { createSimPlatform } from "../dev/sim/platform";
import type { HostProfile } from "../host/link";
import { ContextHeader } from "./ContextHeader";
import { allWord } from "./ComputerMenu";
import { ProjectSheet } from "./ProjectSheet";
import { RouteProvider } from "./Screen";

let app: App;
let release = () => {};
const advance = async (ms = 0) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
async function start(search: string, over: Partial<App> = {}, only?: (p: HostProfile) => boolean) {
  window.history.replaceState(null, "", `/${search}`);
  const platform = createSimPlatform();
  const profiles = (await platform.profiles()).filter(only ?? (() => true));
  const computers = new Computers(platform, profiles);
  release = computers.hold();
  const profile = profiles[0]!;
  const session = computers.session(profile.profileId);
  computers.view(profile.profileId);
  app = { platform, profile, profiles, session, ui: new UiStore(profile.profileId), computers, all: false,
    showAll: vi.fn(), openProject: vi.fn(), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {}, forgetOther: vi.fn(), ...over };
  await advance(1500);
}
const menu = () => screen.getByRole("dialog", { name: "Computers" });

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  prefs.set((p) => ({ ...p, allProjects: true }));
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(() => {
  cleanup(); release(); window.__sim?.host.dispose();
  window.history.replaceState(null, "", "/");
  vi.clearAllTimers(); vi.useRealTimers();
});

it("names the computer in the pill and lists every computer, All and pairing in its menu", async () => {
  await start("?three-computers");
  render(<AppProvider app={app}><ContextHeader /></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: "studio: Live. Choose a computer" }));
  const rows = within(menu()).getAllByRole("button").map((b) => b.textContent);
  expect(rows).toEqual(expect.arrayContaining(["All computersProjects from every computer", "studioConnected", "spareConnected", "laptopConnected"]));
  expect(within(menu()).getByRole("button", { name: /^studio/ }).getAttribute("aria-current")).toBe("true");
  fireEvent.click(within(menu()).getByRole("button", { name: /^spare/ }));
  expect(app.switchTo).toHaveBeenCalledWith("sim-spare");
});

it("chooses All from the menu, and choosing the computer on screen from All shows it alone", async () => {
  await start("?three-computers");
  render(<AppProvider app={app}><ContextHeader /></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: /Choose a computer/ }));
  fireEvent.click(within(menu()).getByRole("button", { name: /^All computers/ }));
  expect(app.showAll).toHaveBeenCalled();
  cleanup();
  app = { ...app, all: true };
  render(<AppProvider app={app}><ContextHeader /></AppProvider>);
  // The pill says All; the project says which computer it is on.
  expect(screen.getByRole("button", { name: "All computers. Choose a computer" })).toBeTruthy();
  expect(document.querySelector(".ctx__computer")?.textContent).toBe("On studio");
  fireEvent.click(screen.getByRole("button", { name: /Choose a computer/ }));
  fireEvent.click(within(menu()).getByRole("button", { name: /^studio/ }));
  expect(app.switchTo).toHaveBeenCalledWith("sim-studio");
});

it("shows each computer's own state, and All counts the ones offline", async () => {
  await start("?three-computers&computer-offline=spare", { all: true });
  render(<AppProvider app={app}><ContextHeader /></AppProvider>);
  expect(screen.getByRole("button", { name: "All computers: 1 offline. Choose a computer" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /Choose a computer/ }));
  expect(within(menu()).getByRole("button", { name: /^spare/ }).textContent).toMatch(/^spareOffline/);
  expect(within(menu()).getByRole("button", { name: /^laptop/ }).textContent).toBe("laptopConnected");
});

it("opens another computer's connection details from its own session", async () => {
  await start("?three-computers");
  render(<AppProvider app={app}><ContextHeader /></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: /Choose a computer/ }));
  fireEvent.click(within(menu()).getByRole("button", { name: "Connection details for laptop" }));
  const details = screen.getByRole("dialog", { name: /^laptop/ });
  expect(within(details).getByText("laptop.tail1234.ts.net:47100")).toBeTruthy();
  // What the details open, they open on that computer.
  fireEvent.click(within(details).getByRole("button", { name: /^Settings/ }));
  expect(app.switchTo).toHaveBeenCalledWith("sim-laptop", { screen: "settings" });
});

it("offers no All with one computer paired", async () => {
  await start("");
  render(<AppProvider app={app}><ContextHeader /></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: /Choose a computer/ }));
  expect(within(menu()).queryByRole("button", { name: /^All computers/ })).toBeNull();
  expect(within(menu()).getByRole("button", { name: /Pair another computer/ })).toBeTruthy();
});

it("lists every computer's projects in All, each naming its computer, and opens one on its computer", async () => {
  await start("?three-computers", { all: true });
  render(<AppProvider app={app}><ProjectSheet open onClose={() => {}} /></AppProvider>);
  const sheet = screen.getByRole("dialog", { name: /^Projects/ });
  expect(within(sheet).getByText("On every computer")).toBeTruthy();
  const row = (name: string) => within(sheet).getAllByRole("button").filter((b) => b.querySelector(".row__title")?.textContent?.startsWith(name));
  // A Home for each computer, the one on screen first.
  expect(row("Home").map((b) => b.querySelector(".all-projects__computer")?.textContent)).toEqual(["studio", "spare", "laptop"]);
  // The same project on two computers is two rows.
  expect(row("field-notes").map((b) => b.querySelector(".all-projects__computer")?.textContent).sort()).toEqual(["spare", "studio"]);
  // Each computer's name is isolated, so it can't turn the path beside it around.
  expect(row("render-farm")[0]!.querySelector(".all-projects__computer")?.tagName).toBe("BDI");
  fireEvent.click(row("render-farm")[0]!);
  expect(app.openProject).toHaveBeenCalledWith("sim-spare", "0192f7a1-0000-7000-8000-000000000203", expect.any(Function));
  // Search reaches a computer's name too.
  fireEvent.change(within(sheet).getByRole("searchbox"), { target: { value: "laptop" } });
  expect(row("travel-journal")).toHaveLength(1);
  expect(row("render-farm")).toHaveLength(0);
});

it("says All is waiting rather than empty while a computer hasn't answered", async () => {
  await start("?three-computers&computer-offline=laptop", { all: true });
  render(<AppProvider app={app}><ProjectSheet open onClose={() => {}} /></AppProvider>);
  expect(screen.getByText("laptop isn’t connected. Its projects appear once it answers.")).toBeTruthy();
});

it("words All's state briefly", () => {
  expect(allWord([{ live: true, offline: false }, { live: true, offline: false }])).toBeNull();
  expect(allWord([{ live: false, offline: false }, { live: true, offline: false }])).toBe("Connecting");
  expect(allWord([{ live: false, offline: true }, { live: false, offline: true }, { live: false, offline: false }])).toBe("2 offline");
});

it("keeps All's pill still while words stream in on every computer", async () => {
  await start("?three-computers", { all: true });
  render(<AppProvider app={app}><ContextHeader /></AppProvider>);
  vi.mocked(freshness).mockClear();
  for (const id of ["sim-studio", "sim-spare", "sim-laptop"])
    for (const at of [3_000_001, 3_000_002]) act(() => app.computers!.peek(id)!.state.set((s) => ({ ...s, lastHeardAt: at })));
  expect(freshness).not.toHaveBeenCalled();
});

it("heads Tasks in All with every project on every computer, until a project is chosen, and offers All projects back", async () => {
  await start("?three-computers", { all: true });
  app.ui.selectTab("work");
  render(<AppProvider app={app}><ContextHeader /></AppProvider>);
  expect(document.querySelector(".ctx__project-name")?.textContent).toBe("All projects");
  expect(document.querySelector(".ctx__computer")?.textContent).toBe("On every computer");
  fireEvent.click(document.querySelector<HTMLElement>(".ctx__context")!);
  const sheet = screen.getByRole("dialog", { name: /^Projects/ });
  expect(within(sheet).getByRole("button", { name: /^All projects/ }).getAttribute("aria-current")).toBe("true");
  fireEvent.click(within(sheet).getAllByRole("button").find((b) => b.querySelector(".row__title")?.textContent?.startsWith("render-farm"))!);
  // All projects is left only once the project is shown, after any switch.
  expect(app.openProject).toHaveBeenCalledWith("sim-spare", "0192f7a1-0000-7000-8000-000000000203", expect.any(Function));
  expect(prefs.get().allProjects).toBe(true);
  act(() => vi.mocked(app.openProject!).mock.calls.at(-1)![2]!());
  expect(prefs.get().allProjects).toBe(false);
  expect(document.querySelector(".ctx__project-name")?.textContent).not.toBe("All projects");
  fireEvent.click(document.querySelector<HTMLElement>(".ctx__context")!);
  fireEvent.click(within(screen.getByRole("dialog", { name: /^Projects/ })).getByRole("button", { name: /^All projects/ }));
  expect(prefs.get().allProjects).toBe(true);
});

it("keeps a hidden Tasks tab on All projects while another tab is shown, each tab deciding for itself", async () => {
  await start("?three-computers", { all: true });
  // Talk is on screen; Tasks stays mounted under it, as the shell keeps it.
  app.ui.selectTab("talk");
  render(<AppProvider app={app}>
    <RouteProvider value={{ tab: "work", route: { screen: "work" }, depth: 0, active: false }}><div data-tab="work"><ContextHeader /></div></RouteProvider>
    <RouteProvider value={{ tab: "talk", route: { screen: "talk" }, depth: 0, active: true }}><div data-tab="talk"><ContextHeader /></div></RouteProvider>
  </AppProvider>);
  expect(document.querySelector('[data-tab="work"] .ctx__project-name')?.textContent).toBe("All projects");
  expect(document.querySelector('[data-tab="talk"] .ctx__project-name')?.textContent).not.toBe("All projects");
});
