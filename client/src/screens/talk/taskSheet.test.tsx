// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { Task } from "../../protocol/types";
import { projectTasks } from "../../state/projection";
import { taskAttentionReason, taskAttentionReasons } from "../work/taskActivity";
import { RouteProvider } from "../../shell/Screen";
import { TalkConversations } from "./Conversations";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });

it("gives every task in a list the reason it gives the task alone", () => {
  const state = app.session.state.get();
  const tasks = Object.values(state.tasks);
  expect(tasks.some((t) => taskAttentionReason(state, t) !== null)).toBe(true);
  expect(taskAttentionReasons(state, tasks)).toEqual(Object.fromEntries(tasks.map((t) => [t.task_id, taskAttentionReason(state, t)])));
});

it("reads the approvals a fixed number of times per update, however many conversations Chat lists", async () => {
  const { session } = app;
  const current = session.state.get().tasks[ids.tDash]!;
  const project = session.state.get().projects[current.project_id]!;
  session.state.set((s) => {
    const more: Record<string, Task> = {};
    for (let i = 0; i < 40; i++) more[`older-${i}`] = { ...current, task_id: `older-${i}`, title: `Older task ${i}`, state: "completed", current_run: null, target_id: null, updated_at: i, conversation: true };
    return { ...s, tasks: { ...s.tasks, ...more } };
  });
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}>
    <TalkConversations project={project} />
  </RouteProvider></AppProvider>);
  expect(screen.getByText("Older task 39")).toBeTruthy();
  expect(projectTasks(session.state.get(), project.project_id).length).toBeGreaterThan(40);

  let reads = 0;
  session.state.set((s) => ({ ...s, approvals: new Proxy(s.approvals, { ownKeys: (target) => { reads++; return Reflect.ownKeys(target); } }) }));
  reads = 0;
  act(() => session.state.set((s) => ({ ...s, lastHeardAt: Date.now() })));
  expect(reads).toBeLessThan(10);
});
