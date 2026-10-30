// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { workIds } from "../../dev/sim/work";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { AttentionList } from "./shared";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

it("opens the failed run's own receipt from its review row after a later run finished", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === workIds.failed)!;
  const failed = host.runs.find(r => r.run_id === task.current_run)!;
  expect(host.attention.some(a => a.kind === "failure" && a.run_id === failed.run_id && a.resolved_at === null)).toBe(true);
  // The person continued the task from Talk; the second run finished.
  const later = { ...failed, run_id: "later-run", state: "completed" as const, admitted_at: failed.admitted_at + 60_000,
    ended_at: failed.admitted_at + 120_000, outcome: { ...failed.outcome!, summary: "Later run result" } };
  host.runs.push(later);
  act(() => {
    host.emit({ type: "run_upserted", run: later });
    host.emit({ type: "task_upserted", task: { ...task, current_run: later.run_id, state: "completed", revision: task.revision + 1 } });
  });
  await tick();
  const route = { screen: "work" } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><AttentionList projectId={task.project_id} review /></RouteProvider></AppProvider>);
  const row = screen.getByText(/^Run 1 failed · run 2 finished/).closest("button")!;
  fireEvent.click(row);
  // The row names run 1's failure, so it must open run 1's receipt, not the
  // newest run's (which would also clear this failure alert unseen).
  expect(app.ui.route()).toEqual({ screen: "receipt", taskId: task.task_id, runId: failed.run_id });
});
