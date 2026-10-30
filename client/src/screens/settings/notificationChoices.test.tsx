// SPDX-License-Identifier: Apache-2.0
// Settings as a page per group, and which alerts reach the person: a default
// for every project, each project's own, and one note per project that leads
// to that project's Needs you.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { parseOpenRoute } from "../../platform/notifications";
import { alertsFor, bundlesFor, withProject } from "../../platform/notificationSettings";
import { RouteProvider } from "../../shell/Screen";
import NeedsYou from "../work/NeedsYou";
import Settings from "./Settings";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
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
const top = () => app.ui.get().stacks[app.ui.get().tab].at(-1);
function show(route: Extract<Route, { screen: "settings" }>): void;
function show(route: Extract<Route, { screen: "needs_you" }>): void;
function show(route: Route) {
  const page: ReactNode = route.screen === "settings" ? <Settings route={route} /> : route.screen === "needs_you" ? <NeedsYou route={route} /> : null;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 1, active: true }}>{page}</RouteProvider></AppProvider>);
}
const kept = () => window.__sim!.host.notificationSettings;

it("reads each project's choice over the default, as the computer does", () => {
  const settings = {
    off: ["finished" as const], bundle: true, revision: 0,
    projects: [{ project_id: "a", mute: false, on: ["finished" as const], off: ["question" as const], bundle: false }, { project_id: "b", mute: true, on: [], off: [] }],
  };
  expect(alertsFor(settings, "other", "finished")).toBe(false);
  expect(alertsFor(settings, "other", "question")).toBe(true);
  expect(alertsFor(settings, "a", "finished")).toBe(true);
  expect(alertsFor(settings, "a", "question")).toBe(false);
  expect(alertsFor(settings, "b", "approval")).toBe(false);
  expect([bundlesFor(settings, "other"), bundlesFor(settings, "a"), bundlesFor(settings, "b")]).toEqual([true, false, true]);
  // A project that says nothing of its own is left out.
  expect(withProject(settings, { project_id: "a", mute: false, on: [], off: [] }).map((p) => p.project_id)).toEqual(["b"]);
});

it("lists Settings as pages, and a link to a section opens its page", async () => {
  show({ screen: "settings" });
  const app_ = within(screen.getByRole("region", { name: "This device" }));
  for (const name of ["Notifications", "Appearance", "Lock", "Warnings", "Data and voice", "Versions"])
    expect(app_.getByRole("button", { name: new RegExp(`^${name}`) })).toBeTruthy();
  const agents = within(screen.getByRole("region", { name: "Agents" }));
  for (const name of ["Default agent and permissions", "Agent apps", "Accounts", "Agent logins"])
    expect(agents.getByRole("button", { name: new RegExp(`^${name}`) })).toBeTruthy();
  const computer = within(screen.getByRole("region", { name: "This computer" }));
  for (const name of ["Connections and readiness", "Open a link from chat"])
    expect(computer.getByRole("button", { name: new RegExp(`^${name}`) })).toBeTruthy();
  expect(screen.queryByText("Reduce motion")).toBeNull();
  fireEvent.click(app_.getByRole("button", { name: /^Appearance/ }));
  expect(top()).toEqual({ screen: "settings", section: "settings-appearance" });
  cleanup();
  // Voice's links from Control land on Data and voice, with Device data beside it.
  show({ screen: "settings", section: "settings-voice" });
  expect(screen.getByRole("heading", { level: 1, name: "Data and voice" })).toBeTruthy();
  expect(screen.getByRole("heading", { name: "Device data" })).toBeTruthy();
  expect(screen.queryByRole("region", { name: "This device" })).toBeNull();
  // Voice control is there too, saying what Jev is; Agent apps no longer holds it.
  await advance();
  expect(screen.getByRole("heading", { name: "Voice control" })).toBeTruthy();
  expect(screen.getByText(/^Jev is the voice service that hears you/)).toBeTruthy();
  cleanup();
  show({ screen: "settings", section: "settings-agents" });
  await advance();
  expect(screen.queryByRole("heading", { name: "Voice control" })).toBeNull();
});

it("turns a topic off for every project, folds alerts per project, and opens a project's own", async () => {
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  const finished = screen.getByRole("checkbox", { name: "Finished tasks" }) as HTMLInputElement;
  expect(finished.checked).toBe(true);
  fireEvent.click(finished); await advance(900);
  expect(kept()).toMatchObject({ off: ["finished"], bundle: false, revision: 1 });
  expect((screen.getByRole("checkbox", { name: "Finished tasks" }) as HTMLInputElement).checked).toBe(false);
  fireEvent.click(screen.getByRole("checkbox", { name: "One notification per project" })); await advance(900);
  expect(kept()).toMatchObject({ off: ["finished"], bundle: true, revision: 2 });
  const projects = within(screen.getByRole("region", { name: "Projects" }));
  fireEvent.click(projects.getByRole("button", { name: /^tidewater-api/ }));
  expect(top()).toEqual({ screen: "settings", section: "settings-notifications", projectId: ids.pTide });
});

it("keeps one project's choices: muted, a topic on over the default, and one notification or each", async () => {
  window.__sim!.host.notificationSettings = { off: ["finished"], bundle: true, projects: [], revision: 4 };
  show({ screen: "settings", section: "settings-notifications", projectId: ids.pTide }); await advance();
  expect(screen.getByText("tidewater-api")).toBeTruthy();
  const finished = within(screen.getByRole("radiogroup", { name: "Finished tasks" }));
  expect(finished.getByRole("radio", { name: "Default (off)" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(finished.getByRole("radio", { name: "On" })); await advance(900);
  expect(kept().projects).toEqual([{ project_id: ids.pTide, mute: false, on: ["finished"], off: [] }]);
  const one = within(screen.getByRole("radiogroup", { name: "One notification for tidewater-api" }));
  expect(one.getByRole("radio", { name: "Default (one)" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(one.getByRole("radio", { name: "Separate" })); await advance(900);
  expect(kept().projects[0]).toMatchObject({ bundle: false });
  fireEvent.click(screen.getByRole("checkbox", { name: "Mute this project" })); await advance(900);
  expect(kept().projects[0]).toMatchObject({ mute: true, on: ["finished"] });
  expect((within(screen.getByRole("radiogroup", { name: "Questions" })).getByRole("radio", { name: "Off" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Reset to the defaults" })); await advance(900);
  expect(kept().projects).toEqual([]);
  expect(kept().revision).toBe(8);
});

it("says to update a computer that doesn't keep these choices", async () => {
  const caps = app.session.state.get().capabilities!;
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...caps, features: caps.features.filter((f) => f !== "notification_settings_v1") } })));
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  expect(screen.getByText(/Update Øphio on .* to choose what alerts you/)).toBeTruthy();
  expect(screen.queryByRole("checkbox", { name: "Finished tasks" })).toBeNull();
  // What this phone does with alerts stays.
  expect(screen.getByRole("heading", { name: "On this phone" })).toBeTruthy();
});

it("opens a project's Needs you from the note that counts its alerts", async () => {
  const route = parseOpenRoute({ kind: "needs_you", profileId: "p1", projectId: ids.pTide });
  expect(route).toEqual({ kind: "needs_you", profileId: "p1", projectId: ids.pTide });
  expect(parseOpenRoute({ kind: "needs_you", profileId: "p1", projectId: "" })).toBeNull();
  show({ screen: "needs_you", projectId: ids.pTide }); await advance();
  expect(screen.getByRole("heading", { level: 1, name: "Needs you" })).toBeTruthy();
  expect(app.ui.get().projectId).toBe(ids.pTide);
  // What finished and nobody has looked at yet is there too.
  expect(screen.getByRole("heading", { name: "To look at" })).toBeTruthy();
  expect(screen.getByText("Add rate-limit headers to /v1/orders")).toBeTruthy();
  expect(screen.getByText("Fix the flaky retry test in the payments client")).toBeTruthy();
});
