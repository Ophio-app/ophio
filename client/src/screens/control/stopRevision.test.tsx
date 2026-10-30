// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import type { CommandEnvelope } from "../../protocol/types";
import { stopStore } from "./stops";
import { StopTask } from "./StopTask";

let app: App;
const advance = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start();
  await advance(900);
});
afterEach(() => {
  cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("Stop still reaches a working run when the agent's own activity lands while Stop is in flight", async () => {
  const host = window.__sim!.host;
  const task = app.session.state.get().tasks[ids.tFlaky]!;
  const runId = task.current_run!;
  // The companion admits stop_task only at the task's current revision
  // (services/commands.rs StopTask, domain::revision), and every agent event
  // bumps that revision (services/runs.rs, harness event handler).
  const original = host.command.bind(host);
  let raced = false;
  vi.spyOn(host, "command").mockImplementation(async (env: CommandEnvelope) => {
    const body = env.body;
    if (body.command !== "stop_task") return original(env);
    const current = host.tasks.find(t => t.task_id === body.task_id)!;
    if (!raced) {
      raced = true;
      // One agent step arrives at the computer just before the Stop does.
      const next = { ...current, revision: current.revision + 1, updated_at: Date.now() };
      host.tasks = host.tasks.map(t => t.task_id === next.task_id ? next : t);
      host.emit({ type: "timeline_appended", task_id: next.task_id, item: { seq: host.seq + 1, at: Date.now(), run_id: runId,
        item: { kind: "activity", tool: "Read", summary: "Read src/retry.rs", status: "succeeded", detail: null } } });
      host.emit({ type: "task_upserted", task: next });
    }
    const now = host.tasks.find(t => t.task_id === body.task_id)!;
    if (env.expected_revision !== null && env.expected_revision !== now.revision)
      return { command_id: env.command_id, status: "rejected", seq: null, result: null,
        error: { code: "stale_revision", message: "A current revision is required", detail: now.revision } };
    return original(env);
  });
  render(<AppProvider app={app}><StopTask task={task} /></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: /^Stop ·/ }));
  await advance(1500);
  const stops = vi.mocked(host.command).mock.calls.filter(([env]) => env.body.command === "stop_task");
  expect(stops.length).toBeGreaterThan(1);
  // The same run is still the task's run, so the person's Stop is carried out
  // rather than refused for the agent's own progress.
  expect(stops.every(([env]) => env.body.command === "stop_task" && env.body.task_id === task.task_id)).toBe(true);
  expect(["stopping", "interrupted"]).toContain(app.session.state.get().runs[runId]?.state);
  expect(screen.queryByText(/A current revision is required/)).toBeNull();
});

it("Stop refused because a later run began is not sent again to that run", async () => {
  const host = window.__sim!.host;
  const task = app.session.state.get().tasks[ids.tFlaky]!;
  const first = app.session.state.get().runs[task.current_run!]!;
  const original = host.command.bind(host);
  vi.spyOn(host, "command").mockImplementation(async (env: CommandEnvelope) => {
    const body = env.body;
    if (body.command !== "stop_task") return original(env);
    const current = host.tasks.find(t => t.task_id === body.task_id)!;
    if (current.current_run === first.run_id) {
      // The run ended and the task continued in a new run before Stop arrived.
      const ended = { ...first, state: "completed" as const, ended_at: Date.now() };
      const later = { ...first, run_id: "later-run", state: "working" as const, admitted_at: Date.now(), ended_at: null };
      host.runs = host.runs.map(r => r.run_id === first.run_id ? ended : r).concat(later);
      const next = { ...current, current_run: later.run_id, state: "working" as const, revision: current.revision + 1, updated_at: Date.now() };
      host.tasks = host.tasks.map(t => t.task_id === next.task_id ? next : t);
      host.emit({ type: "run_upserted", run: ended });
      host.emit({ type: "run_upserted", run: later });
      host.emit({ type: "task_upserted", task: next });
    }
    const now = host.tasks.find(t => t.task_id === body.task_id)!;
    if (env.expected_revision !== null && env.expected_revision !== now.revision)
      return { command_id: env.command_id, status: "rejected", seq: null, result: null,
        error: { code: "stale_revision", message: "A current revision is required", detail: now.revision } };
    return original(env);
  });
  render(<AppProvider app={app}><StopTask task={task} /></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: /^Stop ·/ }));
  await advance(1500);
  expect(vi.mocked(host.command).mock.calls.filter(([env]) => env.body.command === "stop_task")).toHaveLength(1);
  expect(app.session.state.get().runs["later-run"]?.state).toBe("working");
});
