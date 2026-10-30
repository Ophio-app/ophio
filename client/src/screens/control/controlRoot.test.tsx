// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Control from "./Control";
import { displayState } from "./ControlRoot";
import SafetyStrip from "./SafetyStrip";
import { stopStore } from "./stops";
import { controlPart } from "./parts";

// Control's root (D-055, D-066) says who has each display's input once, opens
// on the display someone is driving, shows one kind of thing at a time, the
// chosen project's terminals and previews first, and leaves every Stop to the
// rail.
let app: App;
const tick = async () => act(async () => { await vi.advanceTimersByTimeAsync(900); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick();
  app.ui.selectProject(ids.pTide);
});
afterEach(() => {
  cleanup();
  stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks();
});
async function view() {
  const route = { screen: "control" as const };
  const out = render(<AppProvider app={app}><RouteProvider value={{ tab: "control", depth: 0, active: true, route }}><Control route={route} /></RouteProvider></AppProvider>);
  await tick();
  return out;
}
const dash = () => app.session.state.get().tasks[ids.tDash]!;

it("opens Screens on the display the agent drives, live, with Pause input and Take over but no Stop", async () => {
  const { container } = await view();
  const picker = screen.getByRole("group", { name: "Display" });
  expect(within(picker).getByRole("button", { name: "Screen 1 (DP-1)" }).getAttribute("aria-pressed")).toBe("true");
  const live = container.querySelector<HTMLElement>(".control-live")!;
  expect(live.querySelector(".control-live__state")?.textContent).toBe(`Agent has control · ${dash().title}`);
  expect(within(live).getByRole("button", { name: /^Pause/ })).toBeTruthy();
  expect(within(live).getByRole("button", { name: /Take over/ })).toBeTruthy();
  // The task driving the display isn't listed again under it, and its Stop is the rail's.
  expect(screen.getAllByText(new RegExp(dash().title.slice(0, 20))).length).toBe(1);
  expect(screen.queryByRole("button", { name: /^Stop ·/ })).toBeNull();
  expect(screen.getByRole("button", { name: /Apps and windows/ })).toBeTruthy();

  fireEvent.click(within(picker).getByRole("button", { name: "Screen 2 (HDMI-A-1)" })); await tick();
  const other = container.querySelector<HTMLElement>(".control-live")!;
  expect(other.querySelector(".control-live__state")?.textContent).toBe("No one has input");
  expect(within(other).queryByRole("button", { name: /^Pause/ })).toBeNull();
  expect(within(other).getByRole("button", { name: "Take over" })).toBeTruthy();
});

it("shows one kind of thing at a time, counting the chosen project's, and stops watching on another", async () => {
  const { container } = await view();
  const show = screen.getByRole("radiogroup", { name: "Show" });
  const live = app.session.state.get().terminals.filter((t) => t.state.state !== "exited");
  const terminals = live.filter((t) => t.project_id === ids.pTide);
  const garden = live.filter((t) => t.project_id === ids.pGarden);
  expect(terminals.length).toBeGreaterThan(0);
  expect(garden.length).toBeGreaterThan(0);
  expect(within(show).getByRole("radio", { name: `Terminals ${terminals.length}` })).toBeTruthy();
  // Terminals and Previews are segments, not doors under the picture.
  expect(screen.queryByRole("button", { name: /^Terminals/ })).toBeNull();

  fireEvent.click(within(show).getByRole("radio", { name: /^Terminals/ })); await tick();
  expect(container.querySelector(".control-live")).toBeNull();
  expect(screen.getAllByRole("button", { name: /^(Attach|View output) / }).length).toBe(terminals.length);
  const row = screen.getByRole("button", { name: `Attach ${terminals[0]!.title}` });
  expect(row.textContent).toMatch(/Output .* ago/);
  expect(screen.queryByRole("button", { name: /^Remove/ })).toBeNull();

  // Other projects' sessions are counted, and shown under their names on request.
  expect(screen.queryByRole("button", { name: `Attach ${garden[0]!.title}` })).toBeNull();
  const more = screen.getByRole("button", { name: `Show ${live.length - terminals.length} in other projects` });
  expect(more.getAttribute("aria-expanded")).toBe("false");
  fireEvent.click(more); await tick();
  const group = screen.getByRole("region", { name: "garden-planner" });
  expect(within(group).getByRole("button", { name: `Attach ${garden[0]!.title}` })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Hide other projects" })); await tick();
  expect(screen.queryByRole("region", { name: "garden-planner" })).toBeNull();
});

it("heads Control with the project picker every root has, the computer under it, and Voice", async () => {
  const { container } = await view();
  expect(screen.getByRole("heading", { level: 1, name: "tidewater-api" })).toBeTruthy();
  expect(container.querySelector(".ctx__computer")?.textContent).toBe(app.profile.name);
  expect(screen.getByRole("button", { name: "Control by voice" })).toBeTruthy();
  fireEvent.click(container.querySelector<HTMLElement>(".ctx__context")!); await tick();
  expect(screen.getByRole("dialog", { name: /project/i })).toBeTruthy();
});

it("keeps sessions opened outside any project with Home", async () => {
  app.ui.selectProject(ids.pHome);
  await view();
  const garden = app.session.state.get().terminals.find((t) => t.project_id === ids.pGarden)!;
  act(() => app.session.state.set((s) => ({ ...s, terminals: s.terminals.map((t) => (t.terminal_id === garden.terminal_id ? { ...t, project_id: null } : t)) })));
  await tick();
  fireEvent.click(within(screen.getByRole("radiogroup", { name: "Show" })).getByRole("radio", { name: /^Terminals/ })); await tick();
  expect(screen.getByRole("button", { name: `Attach ${garden.title}` })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /in other projects$/ })); await tick();
  expect(screen.getByRole("region", { name: "tidewater-api" })).toBeTruthy();
});

it("shows the chosen project's previews, the rest on request, and adds to it", async () => {
  app.ui.selectProject(ids.pGarden);
  await view();
  const show = screen.getByRole("radiogroup", { name: "Show" });
  fireEvent.click(within(show).getByRole("radio", { name: "Previews" })); await tick();
  expect(screen.getByText(/in garden-planner\./)).toBeTruthy();
  expect(screen.queryByRole("region", { name: "tidewater-api" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /in other projects$/ })); await tick();
  expect(screen.getByRole("region", { name: "tidewater-api" })).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Add a preview" })); await tick();
  const project = screen.getByRole("combobox", { name: "Project" }) as HTMLSelectElement;
  expect(project.value).toBe(ids.pGarden);
});

it("says a display's input is unknown while offline, and no one's when it is free", () => {
  const state = app.session.state.get();
  expect(displayState(state, state.control[state.targets[0]!.target_id], app.profile.deviceId, false))
    .toEqual({ text: "Who has input is unknown until reconnected", tone: "warn" });
  const free = state.targets.find((t) => state.control[t.target_id]?.input_state === "ready");
  expect(free).toBeTruthy();
  expect(displayState(state, state.control[free!.target_id], app.profile.deviceId, true))
    .toEqual({ text: "No one has input", tone: "neutral" });
});

it("leaves the rail's display lines to the screen that lists displays, keeping every Stop", async () => {
  render(<AppProvider app={app}><SafetyStrip except={{ displays: true }} /></AppProvider>); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^Safety ·/ }));
  expect(screen.queryByText("Screen 1 (DP-1) · Agent is in control")).toBeNull();
  expect(screen.queryByRole("button", { name: /^Pause/ })).toBeNull();
  expect(screen.getByRole("button", { name: /^Stop · Check the staging/ })).toBeTruthy();
});

async function pauseDash() {
  controlPart.set("screens");
  const request = app.session.command({ command: "pause_input", target_id: ids.tgDp1 }, { label: "Pause requested" });
  await tick(); await request; await tick();
}
const actions = (within: HTMLElement) => [...within.querySelectorAll<HTMLButtonElement>(".safety-actions > .btn")];

it("leads a paused agent's display with Resume agent, its review keeping the picture and Mark up, and Take over beside it", async () => {
  await pauseDash();
  const { container } = await view();
  const live = container.querySelector<HTMLElement>(".control-live")!;
  expect(live.querySelector(".control-live__state")?.textContent).toBe("Agent paused · you’re watching until you resume it or take over");
  // A Pause that can't do anything once paused isn't offered.
  expect(actions(live).map((b) => b.textContent)).toEqual(["Resume agent", "Take over"]);
  expect(actions(live)[0]!.classList.contains("btn--primary")).toBe(true);
  fireEvent.click(actions(live)[0]!); await tick();
  const review = within(screen.getByRole("dialog", { name: "Resume the agent on Screen 1 (DP-1)" }));
  expect(review.getByRole("button", { name: "Mark up" })).toBeTruthy();
  expect(review.getByRole("button", { name: "Resume now" })).toBeTruthy();
});

it("leads the paused agent's own screen with Resume agent too, saying what watching, resuming and taking over mean", async () => {
  await pauseDash();
  const { default: ComputerUse, PAUSED_CHOICE } = await import("./ComputerUse");
  const route = { screen: "computer_use" as const, taskId: ids.tDash };
  const { container } = render(<AppProvider app={app}><RouteProvider value={{ tab: "control", depth: 1, active: true, route }}><ComputerUse route={route} /></RouteProvider></AppProvider>);
  await tick();
  const footer = container.querySelector<HTMLElement>(".control-footer")!;
  expect(actions(footer).map((b) => b.textContent)).toEqual(["Resume agent", "Take over"]);
  expect(actions(footer)[0]!.classList.contains("btn--primary")).toBe(true);
  expect(screen.getByText(PAUSED_CHOICE)).toBeTruthy();
  expect(screen.getAllByRole("button", { name: "Resume agent" })).toHaveLength(1);
});
