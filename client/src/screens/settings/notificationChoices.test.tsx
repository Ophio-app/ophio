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
import { ALERT_TYPES, TIER_LABELS } from "../../platform/alertTypes";
import { Toasts } from "../../design/Toast";
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

const group = (name: string) => within(screen.getByRole("radiogroup", { name }));
const checked = (name: string) => group(name).getAllByRole("radio").find((r) => r.getAttribute("aria-checked") === "true")?.textContent;

it("lists every type in plain words under its heading, each with Alert, Silent and Off and the recommended one marked", async () => {
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  for (const heading of ["This phone", "What notifies you", "Projects", "How they look", "Elsewhere"])
    expect(screen.getByRole("heading", { level: 2, name: heading })).toBeTruthy();
  for (const heading of ["Needs you", "Worth a look", "Only in the app"])
    expect(screen.getByRole("heading", { level: 3, name: heading })).toBeTruthy();
  expect(screen.queryByRole("group", { name: "New Øphio versions" })).toBeNull();
  for (const t of ALERT_TYPES.filter((t) => t.type !== "new_version")) {
    const row = screen.getByRole("group", { name: t.name });
    expect(row.getAttribute("aria-describedby") && document.getElementById(row.getAttribute("aria-describedby")!)?.textContent).toBe(t.description);
    const radios = group(t.name).getAllByRole("radio");
    expect(radios.map((r) => r.textContent?.replace(", recommended", ""))).toEqual(["Alert", "Silent", "Off"]);
    // The recommended choice is marked, and chosen at first.
    expect(group(t.name).getByRole("radio", { name: `${TIER_LABELS[t.tier]}, recommended` }).getAttribute("aria-checked")).toBe("true");
  }
  expect(screen.getAllByRole("radio", { name: /, recommended$/ })).toHaveLength(ALERT_TYPES.length - 1);
  // The words come from the computer's own list, in its order.
  expect(ALERT_TYPES.map((t) => t.type)).toContain("app_update");
  expect((screen.getByRole("button", { name: "Use the recommended choices" }) as HTMLButtonElement).disabled).toBe(true);
  // Telegram and Discord have their own settings, and Connections is a tap away.
  expect(screen.getByText(/Telegram or Discord aren’t covered here/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Chat links/ }));
  expect(top()).toEqual({ screen: "connections" });
});

it("puts choices first and keeps background controls in a closed, truthful delivery section", async () => {
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  expect(screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent)).toEqual([
    "What notifies you", "This phone", "Projects", "How they look", "Elsewhere",
  ]);
  const delivery = screen.getByText("Background delivery and phone settings").closest("details")!;
  const summary = delivery.querySelector("summary")!;
  expect(delivery.open).toBe(false);
  expect(summary.textContent).toContain("Alerts are on");
  expect(summary.textContent).toContain("Background delivery stops when Øphio closes.");
  // Folding is presentation only: the watcher keeps updating its status.
  window.__sim!.device.setAlerts({ android: { running: false } });
  await advance(2000);
  expect(summary.textContent).toContain("Android stopped background notifications.");
  expect(delivery.open).toBe(false);
  fireEvent.click(summary);
  expect(delivery.open).toBe(true);
  const toggle = screen.getByRole("checkbox", { name: "Notify me when Øphio is closed" }) as HTMLInputElement;
  expect(toggle.checked).toBe(true);
  fireEvent.click(toggle);
  expect(toggle.checked).toBe(false);
  await advance();
  expect(window.__sim!.device.alerts.settings?.enabled).toBe(false);
  expect(summary.textContent).toContain("Background delivery is off.");
});

it("keeps blocked notification permission visible while delivery details are closed", async () => {
  window.__sim!.device.notificationPermission("blocked");
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  const delivery = screen.getByText("Background delivery and phone settings").closest("details")!;
  expect(delivery.open).toBe(false);
  expect(delivery.querySelector("summary")!.textContent).toContain("Alerts are blocked in the phone’s settings");
  expect(screen.getByRole("radiogroup", { name: "Questions" })).toBeTruthy();
});

it.each([
  ["blocked", "Phone settings block notifications."],
  ["off", "Phone notifications are off."],
  ["unknown", "Notification permission is unknown. Alerts may not arrive."],
  ["stopped", "Android stopped background notifications."],
  ["disabled", "Background delivery is off. Notifications stop when Øphio closes."],
  ["limited", "Background delivery stops when Øphio closes."],
  ["unavailable", "Background delivery status is unavailable."],
  ["disconnected", "Some computers aren’t delivering background notifications."],
  ["unsupported", "This build can’t show system notifications."],
  ["no-watcher", "Notifications arrive only while Øphio is open."],
])("shows a compact %s warning before choices and opens the existing delivery controls", async (state, text) => {
  const device = window.__sim!.device;
  device.setAlerts({ android: { unrestricted: true }, statuses: { [app.profile.profileId]: "watching" } });
  if (state === "blocked") device.notificationPermission("blocked");
  if (state === "off") device.notificationPermission("promptable");
  if (state === "unknown") vi.spyOn(app.platform, "notificationPermission").mockRejectedValue(new Error("Unavailable"));
  if (state === "stopped") device.setAlerts({ android: { running: false } });
  if (state === "disabled") device.setAlerts({ settings: { ...device.alerts.settings!, enabled: false } });
  if (state === "limited") device.setAlerts({ android: { unrestricted: false } });
  if (state === "unavailable") vi.spyOn(app.platform.alerts!, "state").mockRejectedValue(new Error("Unavailable"));
  if (state === "disconnected") device.setAlerts({ statuses: { [app.profile.profileId]: "unreachable" } });
  if (state === "unsupported") delete app.platform.notify;
  if (state === "no-watcher") delete app.platform.alerts;
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  const warning = screen.getByRole("complementary", { name: "Notification delivery limits" });
  expect(within(warning).getByText(text)).toBeTruthy();
  const choices = screen.getByRole("radiogroup", { name: "Questions" });
  expect(warning.compareDocumentPosition(choices) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  const delivery = screen.getByText("Background delivery and phone settings").closest("details")!;
  const scroll = vi.fn();
  delivery.scrollIntoView = scroll;
  expect(delivery.open).toBe(false);
  fireEvent.click(within(warning).getByRole("button", { name: "Delivery settings" }));
  expect(delivery.open).toBe(true);
  expect(document.activeElement).toBe(delivery.querySelector("summary"));
  expect(scroll).toHaveBeenCalledWith({ block: "start" });
});

it("removes the compact warning once delivery is healthy", async () => {
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  expect(screen.getByRole("complementary", { name: "Notification delivery limits" })).toBeTruthy();
  window.__sim!.device.setAlerts({ android: { running: true, unrestricted: true }, statuses: { [app.profile.profileId]: "watching" } });
  await advance(2000);
  expect(screen.queryByRole("complementary", { name: "Notification delivery limits" })).toBeNull();
});

it("changes a type on the tap, tells Android at once, goes back with a toast when refused, and resets to the recommended choices", async () => {
  const device = window.__sim!.device;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "settings", section: "settings-notifications" }, depth: 1, active: true }}><Settings route={{ screen: "settings", section: "settings-notifications" }} /></RouteProvider><Toasts /></AppProvider>);
  await advance();
  fireEvent.click(group("Merged pull requests").getByRole("radio", { name: "Alert" }));
  // Shown on the tap, before Android answers.
  expect(checked("Merged pull requests")).toBe("Alert");
  await advance(0);
  expect(device.tierChanges).toEqual([{ alertType: "merged", tier: "alert" }]);
  expect(device.alerts.tiers?.merged).toBe("alert");
  expect(screen.getByRole("status").textContent).toMatch(/^1 choice differs/);

  device.refuseTier("Android didn’t take it.");
  fireEvent.click(group("Questions").getByRole("radio", { name: "Silent" }));
  expect(checked("Questions")).toBe("Silent");
  await advance(0);
  expect(checked("Questions")).toBe("Alert, recommended");
  expect(screen.getByText("Couldn’t change how questions notify you. Android didn’t take it.")).toBeTruthy();
  expect(device.alerts.tiers?.question).toBe("alert");

  fireEvent.click(screen.getByRole("button", { name: "Use the recommended choices" }));
  expect(checked("Merged pull requests")).toBe("Off, recommended");
  await advance(0);
  expect(device.alerts.tiers?.merged).toBe("off");
});

it("keeps the choices on this device where there is no watcher, and says they apply while Øphio is open", async () => {
  const platform = app.platform as { alerts?: unknown };
  delete platform.alerts;
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  expect(screen.getByText(/notifications arrive only while the app is open/)).toBeTruthy();
  expect(screen.getByText(/while Øphio is open\. A dot marks/)).toBeTruthy();
  fireEvent.click(group("Finished tasks").getByRole("radio", { name: "Silent" }));
  await advance(0);
  expect(checked("Finished tasks")).toBe("Silent");
  expect(JSON.parse(localStorage.getItem("alert-tiers")!)).toMatchObject({ finished: "silent", question: "alert" });
  expect(screen.queryByRole("button", { name: "Open Android settings" })).toBeNull();
});

it("folds alerts per project on the computer, and lists each project with its choices", async () => {
  window.__sim!.host.notificationSettings = { off: [], bundle: false, revision: 3, projects: [
    { project_id: ids.pTide, mute: false, on: [], off: [], tiers: [{ type: "question", tier: "silent" }, { type: "merged", tier: "alert" }] },
  ] };
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  fireEvent.click(screen.getByRole("checkbox", { name: "One notification per project" })); await advance(900);
  expect(kept()).toMatchObject({ bundle: true, revision: 4 });
  const projects = within(screen.getByRole("region", { name: "Projects" }));
  expect(projects.getByRole("button", { name: /^tidewater-api/ }).textContent).toContain("2 changes");
  expect(projects.getByRole("button", { name: /^field-notes/ }).textContent).toContain("Default");
  fireEvent.click(projects.getByRole("button", { name: /^tidewater-api/ }));
  expect(top()).toEqual({ screen: "settings", section: "settings-notifications", projectId: ids.pTide });
});

it("shows a choice the computer keeps for every project as a project's default, and keeps it on save", async () => {
  window.__sim!.host.notificationSettings = { off: [], bundle: false, projects: [], tiers: [{ type: "question", tier: "off" }], revision: 4 };
  show({ screen: "settings", section: "settings-notifications", projectId: ids.pTide }); await advance();
  expect(group("Questions").getByRole("radio", { name: "Default (Off)" }).getAttribute("aria-checked")).toBe("true");
  expect(group("Approvals").getByRole("radio", { name: "Default (Alert)" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(group("Finished tasks").getByRole("radio", { name: "Silent" })); await advance(900);
  expect(kept().tiers).toEqual([{ type: "question", tier: "off" }]);
});

it("keeps one project's choices per type in tiers: muted, a type over the default, and reset", async () => {
  window.__sim!.host.notificationSettings = { off: [], bundle: true, projects: [], revision: 4 };
  show({ screen: "settings", section: "settings-notifications", projectId: ids.pTide }); await advance();
  expect(screen.getByText("tidewater-api")).toBeTruthy();
  // Only types which permit project choices, each with Default showing this phone's choice.
  expect(screen.queryByRole("radiogroup", { name: "New Øphio versions" })).toBeNull();
  expect(screen.queryByRole("radiogroup", { name: "Øphio updates" })).toBeNull();
  expect(screen.getAllByRole("radiogroup").length).toBe(ALERT_TYPES.filter((t) => t.project).length + 1);
  expect(group("Finished tasks").getByRole("radio", { name: "Default (Off)" }).getAttribute("aria-checked")).toBe("true");
  expect(group("Questions").getByRole("radio", { name: "Default (Alert)" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(group("Finished tasks").getByRole("radio", { name: "Alert" }));
  expect(group("Finished tasks").getByRole("radio", { name: "Alert" }).getAttribute("aria-checked")).toBe("true");
  await advance(900);
  expect(kept().projects).toEqual([{ project_id: ids.pTide, mute: false, on: [], off: [], tiers: [{ type: "finished", tier: "alert" }] }]);
  fireEvent.click(group("Questions").getByRole("radio", { name: "Silent" })); await advance(900);
  expect(kept().projects[0]?.tiers).toEqual([{ type: "question", tier: "silent" }, { type: "finished", tier: "alert" }]);
  const one = within(screen.getByRole("radiogroup", { name: "One notification for tidewater-api" }));
  expect(one.getByRole("radio", { name: "Default (one)" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(one.getByRole("radio", { name: "Separate" })); await advance(900);
  expect(kept().projects[0]).toMatchObject({ bundle: false });
  expect(screen.getByRole("status").textContent).toBe("3 changes from the defaults.");
  fireEvent.click(screen.getByRole("checkbox", { name: "Mute this project" })); await advance(900);
  expect(kept().projects[0]).toMatchObject({ mute: true, tiers: [{ type: "question", tier: "silent" }, { type: "finished", tier: "alert" }] });
  expect((group("Questions").getByRole("radio", { name: "Off" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(group("Finished tasks").getByRole("radio", { name: "Default (Off)" }));
  fireEvent.click(screen.getByRole("button", { name: "Reset to the defaults" })); await advance(900);
  expect(kept().projects).toEqual([]);
});

it("keeps the per-topic project page for a computer without types", async () => {
  const caps = app.session.state.get().capabilities!;
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...caps, features: caps.features.filter((f) => f !== "alert_types_v1") } })));
  window.__sim!.host.notificationSettings = { off: ["finished"], bundle: true, projects: [], revision: 4 };
  show({ screen: "settings", section: "settings-notifications", projectId: ids.pTide }); await advance();
  const finished = within(screen.getByRole("radiogroup", { name: "Finished tasks" }));
  expect(finished.getByRole("radio", { name: "Default (off)" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(finished.getByRole("radio", { name: "On" })); await advance(900);
  expect(kept().projects).toEqual([{ project_id: ids.pTide, mute: false, on: ["finished"], off: [] }]);
  cleanup();
  // On the main page, its switches for every project are said, with a way back.
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  expect(screen.getByText(/It doesn’t send finished tasks to any phone/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Turn them back on" })); await advance(900);
  expect(kept().off).toEqual([]);
});

it("says to update a computer that doesn't keep these choices", async () => {
  const caps = app.session.state.get().capabilities!;
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...caps, features: caps.features.filter((f) => f !== "notification_settings_v1" && f !== "alert_types_v1") } })));
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  expect(screen.getByText(/Update Øphio on .* to choose what notifies you from each of its projects/)).toBeTruthy();
  expect(screen.queryByRole("checkbox", { name: "One notification per project" })).toBeNull();
  // What this phone does with notifications stays.
  expect(screen.getByRole("heading", { name: "This phone" })).toBeTruthy();
  expect(screen.getByRole("radiogroup", { name: "Questions" })).toBeTruthy();
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

it("shows the new-version device row only when this computer supports it", async () => {
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  expect(screen.queryByRole("group", { name: "New Øphio versions" })).toBeNull();
  const host = window.__sim!.host;
  act(() => { host.host.capabilities.features.push("release_check_v1"); host.emit({ type: "capabilities_changed", capabilities: host.host.capabilities }); });
  await advance();
  expect(screen.getByRole("group", { name: "New Øphio versions" })).toBeTruthy();
});

it("does not send stored non-project tiers when changing a project", async () => {
  const host = window.__sim!.host;
  host.notificationSettings = { off: [], bundle: false, revision: 0, projects: [{ project_id: ids.pTide, mute: false, on: [], off: [], tiers: [{ type: "new_version", tier: "off" }, { type: "app_update", tier: "silent" }] }] };
  const command = vi.spyOn(app.session, "command");
  show({ screen: "settings", section: "settings-notifications", projectId: ids.pTide }); await advance();
  fireEvent.click(group("Questions").getByRole("radio", { name: "Off" })); await advance(900);
  expect(command).toHaveBeenCalledWith(expect.objectContaining({ command: "set_notification_settings", projects: [expect.objectContaining({ tiers: [{ type: "question", tier: "off" }] })] }), expect.anything());
});

it("does not count or reset the hidden new-version choice", async () => {
  const device = window.__sim!.device;
  device.setAlerts({ tiers: { ...device.alerts.tiers, new_version: "off" } });
  show({ screen: "settings", section: "settings-notifications" }); await advance();
  expect(screen.queryByRole("group", { name: "New Øphio versions" })).toBeNull();
  expect(screen.getByText("These are the recommended choices.")).toBeTruthy();
  fireEvent.click(group("Questions").getByRole("radio", { name: "Silent" }));
  await advance(0);
  expect(screen.getByRole("status").textContent).toMatch(/^1 choice differs/);
  fireEvent.click(screen.getByRole("button", { name: "Use the recommended choices" }));
  await advance(0);
  expect(device.alerts.tiers?.new_version).toBe("off");
  expect(device.tierChanges).not.toContainEqual({ alertType: "new_version", tier: "alert" });
});
