// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { workIds } from "../../dev/sim/work";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import type { Command } from "../../protocol/types";
import Agents from "./Agents";
import { AgentChildren } from "./AgentChildren";
import { StopFacts } from "./AgentFacts";
import { OutsideRow } from "./OutsideAgents";
import { AgentStops, agentStops, stopProgress } from "./agentStops";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "agents" }, depth: 1, active: true }}>{children}</RouteProvider></AppProvider>;
function fixture() {
  const task = app.session.state.get().tasks[workIds.receipt];
  const host = window.__sim?.host;
  if (!task || !host) throw new Error("Missing simulated evidence");
  return { task, host };
}
const commands = (): Command[] => Object.values(app.session.commands.get()).map(c => c.body);
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("shows a scoped Stop under a completed task and keeps its confirmed receipt after navigation", async () => {
  const { task, host } = fixture();
  expect(task.state).toBe("completed");
  const view = render(<AgentChildren task={task} />, { wrapper });
  expect(commands()).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: "Stop helper · Check small-screen spacing" }));
  await advance();
  expect(commands()).toEqual([{ command: "stop_child", run_id: workIds.run, child_id: `${workIds.run}-child` }]);
  expect(host.runs.find(r => r.run_id === workIds.run)?.children[0]?.state).toBe("stopped");
  expect(screen.getByText("Claude Code reported this task stopped.")).toBeTruthy();
  view.unmount();
  render(<AgentChildren task={task} />, { wrapper });
  expect(screen.getByText("Claude Code reported this task stopped.")).toBeTruthy();
  expect(commands()).toHaveLength(1);
});
it("shows the native remedy when separate child Stop is unsupported", () => {
  const { task } = fixture();
  render(<AgentChildren task={task} />, { wrapper });
  expect(screen.getByText(/This helper has no native task control/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: /Stop helper · Preview process/ })).toBeNull();
});
it("does not equate a Stop accepted by the host with a stopped child", async () => {
  const { host } = fixture();
  act(() => host.children.background(true));
  const { task } = fixture();
  render(<AgentChildren task={task} />, { wrapper });
  fireEvent.click(screen.getByRole("button", { name: "Stop helper · Watch integration tests" }));
  await advance();
  expect(screen.getByText(/Still running/)).toBeTruthy();
  expect(host.runs.find(r => r.run_id === workIds.run)?.children[1]?.state).toBe("active");
  expect(screen.getByText(/Claude Code still reports this task after Stop/)).toBeTruthy();
});
it("checks an unknown Stop after restart without sending a second operation", async () => {
  const { task } = fixture();
  act(() => window.__sim?.dropNextReceipt());
  render(<AgentChildren task={task} />, { wrapper });
  fireEvent.click(screen.getByRole("button", { name: "Stop helper · Check small-screen spacing" }));
  await advance();
  expect(commands()).toHaveLength(1);
  const restored = new AgentStops(app.session);
  const key = `${workIds.run}:${workIds.run}-child`;
  expect(restored.get()[key]?.phase).toBe("unknown");
  act(() => window.__sim?.online());
  await advance(1000);
  await act(async () => { const check = restored.check(key); await vi.advanceTimersByTimeAsync(500); await check; });
  expect(restored.get()[key]?.receipt?.facts[0]?.status).toBe("done");
  expect(commands()).toHaveLength(1);
});
it("requires explicit review before stopping an outside process", async () => {
  const { host } = fixture();
  const row = host.children.query().sessions[0];
  if (!row) throw new Error("Missing outside process");
  render(<OutsideRow row={row} refresh={vi.fn()} />, { wrapper });
  expect(screen.getByText("Running outside the app")).toBeTruthy();
  expect(commands()).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: /Review Stop/ }));
  expect(commands()).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: "Stop process group 4242" }));
  await advance();
  expect(commands()).toEqual([{ command: "stop_observed_session", observed_id: row.observed_id, confirmed: true }]);
  expect(screen.getByText("Process group ended")).toBeTruthy();
});
it("blocks outside Stop for expired observations and ordinary task-only grants", () => {
  const { host } = fixture();
  const row = host.children.query().sessions[0];
  if (!row) throw new Error("Missing outside process");
  act(() => app.session.state.set(s => ({ ...s, grants: s.grants ? { ...s.grants, terminal: false } : s.grants })));
  render(<OutsideRow row={{ ...row, observed_at: Date.now() - 31000 }} refresh={vi.fn()} />, { wrapper });
  expect((screen.getByRole("button", { name: /Review Stop/ }) as HTMLButtonElement).disabled).toBe(true);
  expect(screen.getByText(/Task and terminal access/)).toBeTruthy();
  expect(screen.getByText(/Refresh this observation/)).toBeTruthy();
});
it("gives an ignored outside Stop a terminal remedy without force stop", async () => {
  const { host } = fixture();
  const row = host.children.query().sessions[0]!;
  host.children.ignore.add(row.observed_id);
  render(<OutsideRow row={row} refresh={vi.fn()} />, { wrapper });
  fireEvent.click(screen.getByRole("button", { name: /Review Stop/ }));
  fireEvent.click(screen.getByRole("button", { name: "Stop process group 4242" }));
  await advance();
  expect(screen.getByText(/Stop this agent in its terminal on studio/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: /Force stop|Review Stop/ })).toBeNull();
  const receipt = await host.command({ command_id: crypto.randomUUID(), issued_at: Date.now(), expires_at: null, expected_revision: row.revision, body: { command: "stop_observed_session", observed_id: row.observed_id, confirmed: true, force: true } });
  expect(receipt.error?.code).toBe("unsupported");
});
it("keeps children receipt facts independent and labels the unknown branch", () => {
  render(<StopFacts facts={[
    { label: "Child: Build preview", status: "done", detail: "Reported stopped" },
    { label: "Child: Watch tests", status: "still_active", detail: "Ignored Stop" },
    { label: "Child: Unreachable helper", status: "unknown", detail: "Control connection lost" },
  ]} />);
  expect(screen.getAllByRole("listitem")).toHaveLength(3);
  expect(screen.getByText(/Still running/)).toBeTruthy();
  expect(screen.getByText(/Unknown/)).toBeTruthy();
  const current = { id: "id", phase: "accepted" as const, detail: "Accepted" };
  expect(stopProgress(current, "native_acknowledged")).toBe(current);
  expect(stopProgress(current, "reconciliation_required").phase).toBe("unknown");
  expect(agentStops(app.session).get()).toEqual({});
});

it("shows cached process state and disables Stop when the last scan failed", () => {
  const { host } = fixture();
  const row = host.children.query().sessions[0];
  if (!row) throw new Error("Missing outside process");
  render(<OutsideRow row={row} observationFailed refresh={vi.fn()} />, { wrapper });
  expect(screen.getByText("Last observed running outside the app")).toBeTruthy();
  expect((screen.getByRole("button", { name: /Review Stop/ }) as HTMLButtonElement).disabled).toBe(true);
  expect(commands()).toHaveLength(0);
});

it("keeps older-run helper Stop independent of an archived parent’s current run", async () => {
  const { task } = fixture();
  render(<AgentChildren task={{ ...task, state: "archived", current_run: "newer-run" }} />, { wrapper });
  const stop = screen.getByRole("button", { name: "Stop helper · Check small-screen spacing" });
  fireEvent.click(stop);
  fireEvent.click(stop);
  await advance();
  expect(commands()).toEqual([{ command: "stop_child", run_id: workIds.run, child_id: `${workIds.run}-child` }]);
});


it.each(["finished", "marked_completed", "running", "stale_task"] as const)("scopes the steering note to a running turn: %s", async scenario => {
  const { task } = fixture();
  const running = scenario === "running";
  app.ui.selectProject(task.project_id);
  act(() => app.session.state.set(s => {
    const run = s.runs[workIds.run]!;
    return { ...s,
      tasks: { ...s.tasks, [task.task_id]: { ...task, current_run: run.run_id,
        state: running || scenario === "stale_task" ? "working" : "completed" } },
      runs: { ...s.runs, [run.run_id]: { ...run, state: running ? "working" : "completed",
        ended_at: running ? null : Date.now(),
        human_resolution: scenario === "marked_completed"
          ? { resolution: "mark_completed", at: Date.now(), by: { kind: "device", device_id: app.profile.deviceId } } : undefined } },
      harnesses: s.harnesses.map(h => h.kind === task.harness ? { ...h,
        capabilities: { ...h.capabilities, steer_running: { state: "unknown", reason: "Not reported" } } } : h),
    };
  }));
  render(<Agents route={{ screen: "agents" }} />, { wrapper });
  await advance();
  const card = screen.getByRole("heading", { name: task.title }).closest("article")!;
  expect(!!within(card).queryByText(/Steering while running/)).toBe(running);
});
it("counts the ended helpers a run no longer lists", async () => {
  const { task } = fixture();
  const run = app.session.state.get().runs[workIds.run]!;
  const ended = { ...run.children[0]!, child_id: `${run.run_id}-ended`, state: "completed" as const, ended_at: run.admitted_at };
  app.session.state.set(s => ({ ...s, runs: { ...s.runs, [run.run_id]: { ...run, children: [ended], earlier_children: 4870 } } }));
  render(<AgentChildren task={task} />, { wrapper });
  fireEvent.click(screen.getByText("4871 ended helpers"));
  await advance(0);
  expect(screen.getByText("4870 earlier helpers ended and aren’t listed. The computer keeps the most recent.")).toBeTruthy();
});
