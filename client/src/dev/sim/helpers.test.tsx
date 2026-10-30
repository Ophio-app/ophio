// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Receipt from "../../screens/work/Receipt";
import { Transcript } from "../../screens/talk/Transcript";
import { createSimPlatform } from "./platform";
import { workIds } from "./work";

let app: App;
const tick = async (ms = 300) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
function receipt() {
  const route = { screen: "receipt" as const, taskId: workIds.receipt };
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 1, active: true, route }}><Receipt route={route} /></RouteProvider></AppProvider>);
}
it("an observed in-process helper end has no Needs you item", async () => {
  window.__sim!.host.children.endedHelper("in_process"); await tick(); receipt();
  expect(app.session.state.get().tasks[workIds.receipt]!.attention_count).toBe(0);
  expect(screen.getByText(/Summarize the README · Ended with the agent process · Observed on the computer/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Helpers checked" })).toBeNull();
});
it("Helpers checked is a durable human decision, leaving unknown helper facts unchanged", async () => {
  const host = window.__sim!.host;
  host.children.endedHelper("separate_process"); await tick();
  const before = structuredClone(host.runs.find(r => r.run_id === workIds.run)!);
  receipt();
  expect(screen.getByText(/Open this task’s conversation in Claude Code/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Helpers checked" })); await tick(600);
  expect(screen.getByText(/Helpers checked by you · .* · not verified by the computer/)).toBeTruthy();
  const state = app.session.state.get();
  expect(state.tasks[workIds.receipt]!.attention_count).toBe(0);
  expect(state.runs[workIds.run]!.children).toEqual(before.children);
  expect(state.runs[workIds.run]!.state).toBe("completed");
  cleanup();
  render(<AppProvider app={app}><Transcript task={state.tasks[workIds.receipt]!} agent="Claude Code" /></AppProvider>); await tick();
  expect(screen.getByText(/Helpers checked by you · .* · not verified by the computer/)).toBeTruthy();
});
it("offers to ask the task's agent about its unknown helpers, and follows the answer in Chat", async () => {
  window.__sim!.host.children.endedHelper("separate_process"); await tick();
  const command = vi.spyOn(app.session, "command");
  const scroll = vi.fn();
  Element.prototype.scrollIntoView = scroll;
  receipt(); await tick();
  expect(screen.getByText(/The run ended before the computer heard how this helper finished\./)).toBeTruthy();
  expect(screen.queryByText(/Check the helper details and output/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "See the helpers" }));
  expect(scroll.mock.contexts.at(-1)).toBe(screen.getByRole("heading", { name: "Still running / unknown" }));
  fireEvent.click(screen.getByRole("button", { name: "Summarize what happened" })); await tick(600);
  expect(command.mock.calls.map(([c]) => c).find((c) => c.command === "resume_task")).toMatchObject({ task_id: workIds.receipt, read_only: true });
  const ui = app.ui.get();
  expect(ui.tab).toBe("talk");
  expect(ui.taskByProject[ui.projectId!]).toBe(workIds.receipt);
});
