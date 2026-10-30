// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { delayedDecision } from "../../dev/sim/talkWork";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { unresolvedAttention as openAttention } from "../../state/projection";
import Receipt from "./Receipt";
import { TalkHeader } from "../talk/TalkHeader";
import { Transcript } from "../talk/Transcript";
import { AttentionList, TaskRow } from "./shared";
import { RunPicker } from "./RunPicker";


let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
function view(child: React.ReactNode, route: Route = { screen: "talk" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{child}</RouteProvider></AppProvider>);
}

it.each(["mark_completed", "mark_failed"] as const)("labels a human %s in Chat, Tasks and run history and clears helper attention", async resolution => {
  const host = window.__sim!.host;
  const taskId = delayedDecision(host);
  const before = host.tasks.find(t => t.task_id === taskId)!;
  const runBefore = host.runs.find(r => r.run_id === before.current_run)!;
  const item = { attention_id: "helper-unknown", kind: "reconciliation_required" as const, task_id: taskId, run_id: null, approval_id: null, title: "Helper outcome unknown · A helper", body: null, priority: 1, created_at: Date.now(), resolved_at: null };
  host.attention.push(item); host.emit({ type: "attention_upserted", item });
  const decision = app.session.command({ command: "reconcile_task", task_id: taskId, resolution }, { label: "Decide", expectedRevision: before.revision });
  await tick(600); await decision;
  const task = app.session.state.get().tasks[taskId]!;
  const run = app.session.state.get().runs[runBefore.run_id]!;
  const project = app.session.state.get().projects[task.project_id]!;
  const label = resolution === "mark_completed" ? "Marked completed" : "Marked failed";
  expect(task.attention_count).toBe(0);
  expect(openAttention(app.session.state.get()).filter(a => a.task_id === taskId)).toHaveLength(0);
  expect(run.children[0]?.state).toBe("unknown");
  view(<><TalkHeader project={project} task={task} run={run} /><TaskRow task={task} /><Transcript task={task} agent="Fixture" /></>);
  await tick();
  expect(screen.getAllByText(new RegExp(label)).length).toBeGreaterThanOrEqual(3);
  expect(screen.getByText(new RegExp(`${label} by you · .* · not verified by the computer`))).toBeTruthy();
  // Back from a task goes to Work, where the task's row says the same.
  fireEvent.click(document.querySelector('.chat-head__back')!);
  expect(app.ui.get().tab).toBe("work");
  expect(document.querySelector(".work-row-state")?.textContent).not.toMatch(resolution === "mark_completed" ? /Finished/ : /Failed ·/);
  cleanup();
  const older = { ...run, run_id: "older", human_resolution: undefined, admitted_at: run.admitted_at - 60_000 };
  view(<RunPicker task={task} runs={[run, older]} latest={run} selectedId={null} onChange={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: new RegExp(`^Choose a run: showing Run 2, Work, ${label},`) }));
  expect(screen.getAllByRole("button", { name: new RegExp(`^Run 2, Work, ${label}, started .*, latest$`) })).toHaveLength(1);
  expect(screen.getAllByRole("button", { name: /^Run 1, / })).toHaveLength(1);
  expect(screen.queryByRole("button", { name: /^Latest run ·/ })).toBeNull();
  cleanup();
  view(<AttentionList projectId={task.project_id} />);
  expect(screen.queryByText(task.title)).toBeNull();
  cleanup();
  view(<Receipt route={{ screen: "receipt", taskId }} />);
  expect(screen.getByText(/Your decision covers this run and its helper unknowns/)).toBeTruthy();
});
