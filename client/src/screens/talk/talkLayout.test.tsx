// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import SafetyStrip from "../control/SafetyStrip";
import { stopStore } from "../control/stops";
import Talk from "./Talk";

// A running task's Talk keeps to the conversation, how a message is sent and
// which agent reads it, the task's state, and Stop.
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
  app.ui.selectTask(ids.pTide, ids.tDash);
});
afterEach(() => { cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
function view() {
  const route = { screen: "talk" as const };
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /></RouteProvider></AppProvider>);
}

it("puts the task's agent beside how a message is sent and its launch flags only in its sheet", async () => {
  const { container } = view(); await tick();
  const chips = container.querySelector<HTMLElement>(".composer__chips")!;
  const [how, agent] = within(chips).getAllByRole("button");
  expect(how?.textContent).toBe("How to send: Steer now");
  expect(agent?.textContent).toBe("Agent: Claude Code · Opus 5.5 · High");
  expect(screen.queryByText(/Permission prompts|Send to/)).toBeNull();

  fireEvent.click(agent!);
  const sheet = screen.getByRole("dialog", { name: "This task’s agent" });
  expect(within(sheet).getByText("Claude Code · Opus 5.5 · High")).toBeTruthy();
  expect(within(sheet).getByText("Permission prompts")).toBeTruthy();
  expect(within(sheet).getByRole("button", { name: /New task/ })).toBeTruthy();
});

it("shows Stop as the symbol alone, still named for its task and display", async () => {
  const { container } = view(); await tick();
  const bar = container.querySelector<HTMLElement>(".chat-head")!;
  const stop = within(bar).getByRole("button", { name: /^Stop · Check the staging/ });
  expect(stop.textContent).toBe("");
  expect(stop.getAttribute("aria-label")).toMatch(/ · Screen 1 \(DP-1\)$/);
  expect(bar.querySelector(".chat-head__where")?.textContent).toBe("on Screen 1 (DP-1)");
});

it("drops this task's agent when the message will start another task", async () => {
  const { container } = view(); await tick();
  fireEvent.click(within(container.querySelector<HTMLElement>(".composer__chips")!).getByRole("button", { name: "How to send: Steer now" }));
  fireEvent.click(screen.getByRole("radio", { name: /Add to Next up/ })); await tick();
  expect(screen.queryByRole("button", { name: /^Agent: / })).toBeNull();
});

it("leaves the rail without Chat's task but with that task's display and Pause input", async () => {
  render(<AppProvider app={app}><SafetyStrip except={{ taskId: ids.tDash, keepDisplay: true }} /></AppProvider>); await tick();
  expect(screen.queryByRole("button", { name: /^Stop · Check the staging/ })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /^Safety ·/ }));
  expect(screen.getByText("Screen 1 (DP-1) · Agent is in control")).toBeTruthy();
  expect(screen.getByRole("button", { name: /^Pause/ }).textContent).toBe("Pause agent");
});
