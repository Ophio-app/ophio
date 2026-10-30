// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import ProjectSheet from "./ProjectSheet";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const close = vi.fn();
function show() { return render(<AppProvider app={app}><ProjectSheet project={undefined} onClose={close} /></AppProvider>); }
async function projects() {
  show(); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Open Projects" })); await advance();
}
function selectTwo() {
  fireEvent.click(screen.getByRole("checkbox", { name: "Select weather-station" }));
  fireEvent.click(screen.getByRole("checkbox", { name: "Select weekend-site" }));
  fireEvent.click(screen.getByRole("button", { name: "Review selection (2)" }));
}
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

it("browses the computer without a typed path, then confirms only selected folders", async () => {
  const before = window.__sim!.host.projects.length;
  await projects();
  expect(screen.queryByLabelText("Folder on computer")).toBeNull();
  expect((screen.getByRole("checkbox", { name: "Select field-notes" }) as HTMLInputElement).disabled).toBe(true);
  selectTwo();
  expect(screen.getByText("/home/sam/Projects/weather-station")).toBeTruthy();
  expect(window.__sim!.host.projects).toHaveLength(before);
  fireEvent.click(screen.getByRole("button", { name: "Add 2 projects" })); await advance();
  expect(window.__sim!.host.projects).toHaveLength(before + 2);
  expect(close).toHaveBeenCalledTimes(1);
});

it("selects visible eligible folders, filters and shows hidden folders explicitly", async () => {
  await projects();
  fireEvent.click(screen.getByRole("button", { name: "Select folders on this page" }));
  expect(screen.getByRole("button", { name: "Review selection (2)" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Clear selection (2)" }));
  fireEvent.click(screen.getByText("Filter folders"));
  fireEvent.click(screen.getByRole("checkbox", { name: "Show hidden folders" })); await advance();
  expect(screen.getByRole("button", { name: "Open .archive" })).toBeTruthy();
  fireEvent.change(screen.getByRole("textbox", { name: "Find a folder here" }), { target: { value: "WEATHER" } }); await advance();
  expect(screen.getByRole("button", { name: "Open weather-station" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Open weekend-site" })).toBeNull();
});

it("navigates into a project and selects the current folder", async () => {
  await projects();
  fireEvent.click(screen.getByRole("button", { name: "Open weather-station" })); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Select this folder" }));
  expect(screen.getByRole("button", { name: "Review selection (1)" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Projects" })); await advance();
  expect(screen.getByRole("button", { name: "Open weekend-site" })).toBeTruthy();
});

it("does not send a new query to an old companion, and keeps manual entry", async () => {
  // The old cached snapshot still claims support; current authentication wins.
  app.session.state.set(s => ({ ...s, capabilities: { ...s.capabilities!, features: [] } }));
  const query = vi.spyOn(app.session, "query");
  show(); await advance();
  expect(screen.getByText(/Update Ophio on the computer/)).toBeTruthy();
  expect(screen.getByLabelText("Folder on computer")).toBeTruthy();
  expect(query.mock.calls.some(([q]) => q.kind === "project_folders")).toBe(false);
});

it("withholds browsing without Manage permission, including direct query attempts", async () => {
  window.__sim!.permissions({ admin: false }); await advance(900);
  show(); await advance();
  expect(screen.getByText(/need this device’s Manage permission/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Open Home" })).toBeNull();
  const result = app.session.query({ kind: "project_folders", path: null, after: null, search: "", show_hidden: false }).catch(e => e);
  await advance();
  expect((await result).error.code).toBe("permission_denied");
});

it("stops a batch on an unknown receipt instead of adding or retrying the rest", async () => {
  await projects(); selectTwo();
  const command = vi.spyOn(app.session, "command");
  window.__sim!.dropNextReceipt();
  fireEvent.click(screen.getByRole("button", { name: "Add 2 projects" })); await advance();
  expect(command).toHaveBeenCalledTimes(1);
  expect(screen.getByText(/0 of 2 confirmed added. Stopped here/)).toBeTruthy();
  expect(close).not.toHaveBeenCalled();
  await advance(7000);
  expect(command).toHaveBeenCalledTimes(1);
});

it("stops after closing even when an already-sent addition succeeds", async () => {
  await projects(); selectTwo();
  const command = vi.spyOn(app.session, "command");
  window.__sim!.latency(500);
  fireEvent.click(screen.getByRole("button", { name: "Add 2 projects" }));
  cleanup(); await advance(1000);
  expect(command).toHaveBeenCalledTimes(1);
  expect(close).not.toHaveBeenCalled();
});

it("stops remaining additions when Manage access is withdrawn mid-batch", async () => {
  await projects(); selectTwo();
  const command = vi.spyOn(app.session, "command");
  window.__sim!.latency(500);
  fireEvent.click(screen.getByRole("button", { name: "Add 2 projects" }));
  await act(async () => { window.__sim!.permissions({ admin: false }); });
  await advance(1800);
  expect(command).toHaveBeenCalledTimes(1);
  expect(screen.getByText(/need this device’s Manage permission/)).toBeTruthy();
  expect(close).not.toHaveBeenCalled();
});

it("reports partial success without repeating an already confirmed addition", async () => {
  await projects(); selectTwo();
  const host = window.__sim!.host;
  const real = host.command.bind(host);
  vi.spyOn(host, "command").mockImplementation(async env => env.body.command === "add_project" && env.body.host_path.endsWith("weekend-site")
    ? { command_id: env.command_id, status: "rejected", seq: null, result: null, error: { code: "not_found", message: "Folder removed" } }
    : real(env));
  fireEvent.click(screen.getByRole("button", { name: "Add 2 projects" })); await advance();
  expect(screen.getByText(/1 of 2 confirmed added/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Add 1 project" })).toBeTruthy();
  expect(host.projects.filter(p => p.name === "weather-station")).toHaveLength(1);
  expect(close).not.toHaveBeenCalled();
});

it("requests the next page explicitly and keeps selections across pages", async () => {
  await projects();
  const real = app.session.query.bind(app.session);
  const query = vi.spyOn(app.session, "query").mockImplementation(q => {
    if (q.kind !== "project_folders") return real(q);
    const folder = { name: q.after ? "later-folder" : "first-folder", path: q.after ? "/one/later" : "/one/first", project_id: null, unavailable: null };
    return Promise.resolve({ kind: "project_folders", listing: { current: { ...folder, name: "one", path: "/one" }, ancestors: [], folders: [folder], next_after: q.after ? null : "first-folder" } } as never);
  });
  fireEvent.click(screen.getByRole("button", { name: "Open weather-station" })); await advance();
  fireEvent.click(screen.getByRole("checkbox", { name: "Select first-folder" }));
  fireEvent.click(screen.getByRole("button", { name: "Next folders" })); await advance();
  expect(query.mock.calls.some(([q]) => q.kind === "project_folders" && q.after === "first-folder")).toBe(true);
  fireEvent.click(screen.getByRole("checkbox", { name: "Select later-folder" }));
  expect(screen.getByRole("button", { name: "Review selection (2)" })).toBeTruthy();
});

it("discards stale location responses and recovers after a listing error", async () => {
  await projects();
  const original = app.session.query.bind(app.session);
  let release: (() => void) | undefined;
  vi.spyOn(app.session, "query").mockImplementation(q => {
    if (q.kind === "project_folders" && q.path?.endsWith("weather-station")) return new Promise(resolve => { release = () => resolve({ kind: "project_folders", listing: { current: null, folders: [], ancestors: [], next_after: null } } as never); });
    return original(q);
  });
  fireEvent.click(screen.getByRole("button", { name: "Open weather-station" })); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Locations" })); await advance();
  await act(async () => release!());
  expect(screen.getByRole("button", { name: "Open Home" })).toBeTruthy();
  vi.mocked(app.session.query).mockRejectedValueOnce(new Error("Drive disconnected"));
  fireEvent.click(screen.getByRole("button", { name: "Open Projects" })); await advance();
  expect(screen.getByText("Drive disconnected")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Try again" })); await advance();
  expect(screen.getByRole("button", { name: "Open weekend-site" })).toBeTruthy();
});
