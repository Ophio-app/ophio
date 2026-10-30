// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { RouteProvider } from "../../shell/Screen";
import Control from "./Control";

let app: App;
const tick = async (ms = 400) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  const host = window.__sim!.host;
  host.terminals = [{ ...host.terminals[0]!, title: "Running shell" }, { ...host.terminals[0]!, terminal_id: "ended", title: "Finished shell", state: { state: "exited", code: 0 } }];
  host.emit({ type: "terminals_changed", terminals: host.terminals }); await tick();
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
// Control's terminals are its Terminals segment.
function mount() {
  const route = { screen: "control" as const };
  app.ui.selectTab("control");
  const view = render(<AppProvider app={app}><RouteProvider value={{ tab: "control", depth: 0, active: true, route }}><Control route={route} /></RouteProvider></AppProvider>);
  fireEvent.click(screen.getByRole("radio", { name: /^Terminals/ }));
  return view;
}
it("running sessions Attach; exited sessions View output and Remove", async () => {
  mount(); await tick();
  expect(screen.getAllByRole("button", { name: "Attach Running shell" })).toHaveLength(1);
  fireEvent.click(screen.getByRole("button", { name: "View output Finished shell" }));
  expect(app.ui.route()).toEqual({ screen: "terminal", terminalId: "ended" });
  fireEvent.click(screen.getByRole("button", { name: "Attach Running shell" }));
  expect(app.ui.route()).toEqual({ screen: "terminal", terminalId: window.__sim!.host.terminals[0]!.terminal_id });
});
it("removing an ended session keeps it off this device's list after a fresh query", async () => {
  mount(); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Remove Finished shell" }));
  expect(screen.queryByText("Finished shell")).toBeNull();
  expect(screen.getByRole("button", { name: "Attach Running shell" })).toBeTruthy();
  cleanup(); mount(); await tick();
  expect(screen.queryByText("Finished shell")).toBeNull();
  expect(screen.getByRole("button", { name: "Attach Running shell" })).toBeTruthy();
  expect(Object.values(app.session.commands.get())).toHaveLength(0);
});
it("a new terminal says it is opening while the computer is slow to answer", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  mount(); await tick();
  fireEvent.click(screen.getByRole("button", { name: "New terminal" }));
  window.__sim!.latency(5000);
  fireEvent.click(screen.getByRole("button", { name: "Open new terminal" }));
  await tick(1000);
  expect(screen.getByRole("button", { name: "Opening terminal…" }).getAttribute("aria-busy")).toBe("true");
  await tick(5000);
  expect(app.ui.route()).toMatchObject({ screen: "terminal" });
});

it("gives ordinary exits an exit code and a stable observation time", async () => {
  const view = mount(); await tick();
  const line = screen.getByText(/Exited with code 0 · observed/).textContent;
  expect(line).toMatch(/\d{1,2}:\d{2}/);
  view.unmount(); await tick(60_000); mount(); await tick();
  expect(screen.getByText(/Exited with code 0 · observed/).textContent).toBe(line);
});

it("shows a confirmed user close in Control after reopening the list", async () => {
  mount(); await tick();
  const terminal = window.__sim!.host.terminals[0]!;
  act(() => { void app.session.command({ command: "close_terminal", terminal_id: terminal.terminal_id }); });
  await tick(800);
  expect(screen.getByText(/Closed by you ·/).textContent).toMatch(/\d{1,2}:\d{2}/);
  cleanup(); mount(); await tick();
  expect(screen.getByText(/Closed by you ·/)).toBeTruthy();
  expect(screen.getByText(/Exited with code 0 · observed/)).toBeTruthy();
});
