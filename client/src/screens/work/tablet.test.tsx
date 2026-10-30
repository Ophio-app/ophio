// SPDX-License-Identifier: Apache-2.0
import { useState, type ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { StatusSheet, availabilityStatus, checkText, connectionCheck } from "../../shell/StatusSheet";
import type { ReadinessCheck as Check } from "../../protocol/types";
import { ReadinessCheck } from "./ReadinessCheck";
import Agents from "./Agents";
import Control, { computerUseBlocker } from "../control/Control";
import { TerminalModeBar } from "../control/Terminal";
import Talk from "../talk/Talk";
import Review from "../talk/Review";
import { buildBrief, digestSentences } from "./brief";

vi.mock("@xterm/xterm", () => ({ Terminal: class {} }));
vi.mock("@xterm/addon-fit", () => ({ FitAddon: class {} }));
let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "agents" }, depth: 1, active: true }}>{children}</RouteProvider></AppProvider>;
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
  app.ui.selectTask(ids.pTide, null);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});
const unavailable = () => act(() => window.__sim?.host.tablet.unavailable());
const commands = () => Object.values(app.session.commands.get()).map(c => c.body);

it("punctuates distinct checks and suppresses duplicate detail/remedy from older computers", () => {
  expect(checkText("Install the tool", "Install the tool.")).toBe("Install the tool.");
  expect(checkText("Not installed", "Install the tool")).toBe("Not installed. Install the tool.");
  expect(checkText(null, "", "Ready!")).toBe("Ready!");
  expect(availabilityStatus({ state: "permission_missing", remedy: "Set up capture" })).toBe("info");
  expect(availabilityStatus({ state: "unsupported", reason: "Absent" })).toBe("info");
});
it("uses a current device connection as evidence without claiming every address is reachable", () => {
  const check: Check = { area: "network", label: "Private network reachability", status: "unknown", detail: "Not checked", remedy: null };
  expect(connectionCheck(check, true, ["host:47100"]).detail).toContain("This device reached the computer at host:47100.");
  expect(connectionCheck(check, true, ["one", "two"]).detail).toContain("one of its paired addresses");
  expect(connectionCheck(check, false, ["host:47100"])).toEqual(check);
});
it("reports the same missing approval ability as Agents, neutral setup, and supported tools only", () => {
  unavailable();
  render(<StatusSheet open onClose={vi.fn()} />, { wrapper });
  const agents = screen.getByText("Agent tools").closest("section");
  if (!agents) throw new Error("Agent tools section missing");
  expect(agents.textContent).toContain("Claude permission prompts");
  expect(agents.textContent).not.toContain("Passed");
  expect(agents.textContent).not.toContain("Hermes");
  expect(agents.textContent).not.toContain("Failed");
  expect(screen.getAllByText("Copy command").length).toBeGreaterThan(0);
  expect(screen.getByText(/This device reached the computer at/)).toBeTruthy();
  const checks = screen.getByText("Checks").closest("section");
  expect(checks?.textContent?.match(/Install computer-mcp/g)).toHaveLength(1);
});
it("waits for observed results after admission, including across remount", async () => {
  unavailable();
  const view = render(<ReadinessCheck />, { wrapper });
  expect(commands()).toEqual([]);
  fireEvent.click(screen.getByRole("button", { name: "Check again" }));
  expect(screen.getByRole("button", { name: "Checking…" }).hasAttribute("disabled")).toBe(true);
  view.unmount();
  render(<ReadinessCheck />, { wrapper });
  expect(screen.queryByText("Checks updated.")).toBeNull();
  await advance();
  expect(screen.getByText("Checks updated.")).toBeTruthy();
  expect(commands()).toEqual([{ command: "run_readiness_check" }]);
});
it("keeps a failed readiness result inline and permits an explicit retry", async () => {
  if (!window.__sim) throw new Error("Sim missing");
  window.__sim.host.tablet.failCheck = true;
  render(<ReadinessCheck />, { wrapper });
  fireEvent.click(screen.getByRole("button", { name: "Check again" }));
  await advance();
  expect(screen.getByText(/The computer couldn’t finish the agent checks/)).toBeTruthy();
  expect(screen.queryByText("Checks updated.")).toBeNull();
  window.__sim.host.tablet.failCheck = false;
  fireEvent.click(screen.getByRole("button", { name: "Check again" }));
  await advance();
  expect(screen.getByText("Checks updated.")).toBeTruthy();
});
it("shows an unknown check after a lost receipt and disables checks while offline", async () => {
  render(<ReadinessCheck />, { wrapper });
  act(() => window.__sim?.dropNextReceipt());
  fireEvent.click(screen.getByRole("button", { name: "Check again" }));
  await advance();
  expect(screen.getByText("Outcome unknown")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Check again" }).hasAttribute("disabled")).toBe(true);
  expect(commands()).toHaveLength(1);
});
it("offers Check again on Agents and replaces a repaired setup notice with the result", async () => {
  unavailable();
  render(<Agents route={{ screen: "agents" }} />, { wrapper });
  expect(screen.getByText("Claude Code: needs setup")).toBeTruthy();
  act(() => window.__sim?.host.tablet.repairOnCheck());
  fireEvent.click(screen.getByRole("button", { name: "Check again" }));
  await advance();
  expect(screen.queryByText("Claude Code: needs setup")).toBeNull();
  expect(screen.getByText("Checks updated.")).toBeTruthy();
});
it.each(["talk", "review"] as const)("offers Check again on the %s no-agent notice", async page => {
  unavailable();
  app.ui.saveDraft(ids.pTide, null, { text: "Review the project" });
  render(page === "talk" ? <Talk route={{ screen: "talk" }} /> : <Review route={{ screen: "review", projectId: ids.pTide, taskId: null }} />, { wrapper });
  expect(screen.getByText("No agent can start work here yet")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Check again" }));
  await advance();
  expect(commands().filter(c => c.command === "run_readiness_check")).toHaveLength(1);
  expect(app.ui.draft(ids.pTide, null)?.text).toBe("Review the project");
});
it("shows nothing of computer use that can't start, and shows the host’s terminal output age", async () => {
  unavailable();
  render(<Control route={{ screen: "control" }} />, { wrapper });
  await advance();
  expect(screen.queryByRole("button", { name: /computer use/ })).toBeNull();
  expect(screen.queryByText("No desktop tasks running")).toBeNull();
  expect(screen.queryByText(/Claude permission prompts/)).toBeNull();
  fireEvent.click(screen.getByRole("radio", { name: /^Terminals/ }));
  expect(screen.queryByText(/Last output time not reported/)).toBeNull();
  expect(screen.getAllByText(/Output/).length).toBeGreaterThan(0);
  const state = app.session.state.get();
  expect(computerUseBlocker(false, ids.pTide, state.capabilities, state.grants)).toContain("Reconnect");
});
it("toggles the terminal keyboard from actual viewport visibility and hides the focused field", async () => {
  Object.defineProperty(window, "innerWidth", { configurable: true, value: 390 });
  Object.defineProperty(window, "innerHeight", { configurable: true, value: 844 });
  function Controls() {
    const [mode, setMode] = useState<"text" | "keys">("keys");
    return <><textarea aria-label="Terminal input" /><TerminalModeBar mode={mode} onChange={setMode} inputReady showKeyboard={() => screen.getByLabelText("Terminal input").focus()} /></>;
  }
  render(<Controls />);
  fireEvent(window, new Event("resize"));
  fireEvent.click(screen.getByRole("button", { name: "Show keyboard" }));
  await advance(60);
  // Focus alone does not claim that a soft keyboard opened.
  expect(screen.getByRole("button", { name: "Show keyboard" })).toBeTruthy();
  Object.defineProperty(window, "innerHeight", { configurable: true, value: 480 });
  fireEvent(window, new Event("resize"));
  expect(screen.getByRole("button", { name: "Hide keyboard" })).toBeTruthy();
  expect(within(screen.getByRole("radiogroup")).getByRole("radio", { name: "Text mode" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Hide keyboard" }));
  await advance(60);
  expect(document.activeElement).not.toBe(screen.getByLabelText("Terminal input"));
  expect(screen.getByRole("button", { name: "Show keyboard" })).toBeTruthy();
});
it("gives an empty work brief a complete sentence", () => {
  expect(digestSentences(buildBrief("project", 0, [], [], [])).lead).toBe("No new task results to show.");
});
