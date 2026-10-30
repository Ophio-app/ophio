// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

vi.mock("./freshness", async (original) => {
  const actual = await original<typeof import("./freshness")>();
  return { ...actual, freshness: vi.fn(actual.freshness) };
});
vi.mock("../screens/work/taskActivity", async (original) => {
  const actual = await original<typeof import("../screens/work/taskActivity")>();
  return { ...actual, taskAttentionReasons: vi.fn(actual.taskAttentionReasons) };
});
vi.mock("../state/projection", async (original) => {
  const actual = await original<typeof import("../state/projection")>();
  return { ...actual, openAttention: vi.fn(actual.openAttention) };
});

import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { HostSession } from "../host/session";
import { TalkHeader } from "../screens/talk/TalkHeader";
import { taskAttentionReasons } from "../screens/work/taskActivity";
import { openAttention } from "../state/projection";
import { ContextHeader } from "./ContextHeader";
import { freshness } from "./freshness";

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

const streaming = async () => {
  for (const at of [3_000_001, 3_000_002, 3_000_003]) act(() => app.session.state.set((s) => ({ ...s, lastHeardAt: at })));
  await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
};

it("keeps the header's status and its closed sheets still while words stream and the clock ticks", async () => {
  render(<AppProvider app={app}><ContextHeader /></AppProvider>);
  expect(screen.getByRole("button", { name: /Connection details/ })).toBeTruthy();
  vi.mocked(freshness).mockClear();
  vi.mocked(openAttention).mockClear();
  await streaming();
  expect(freshness).not.toHaveBeenCalled();
  expect(openAttention).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: /Connection details/ }));
  expect(screen.getByText("Private address")).toBeTruthy();
});

it("lists no tasks in the chat header, even with its ⋯ open, while words stream", async () => {
  const task = app.session.state.get().tasks[ids.tDash]!;
  const project = app.session.state.get().projects[task.project_id]!;
  render(<AppProvider app={app}><TalkHeader project={project} task={task} run={undefined} /></AppProvider>);
  vi.mocked(taskAttentionReasons).mockClear();
  await streaming();
  expect(taskAttentionReasons).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "More about this task" }));
  expect(screen.getByText("What happened in this task")).toBeTruthy();
  await streaming();
  expect(taskAttentionReasons).not.toHaveBeenCalled();
});

it("marks the project switcher when another project has something unread", async () => {
  const current = app.ui.get().projectId;
  const other = Object.values(app.session.state.get().tasks).find(t => t.project_id !== current && t.state !== "archived")!;
  act(() => app.session.state.set((s) => ({ ...s, attention: {} })));
  render(<AppProvider app={app}><ContextHeader /></AppProvider>);
  expect(screen.queryByText(/waiting in other projects/)).toBeNull();
  const item = { attention_id: "done", kind: "completed" as const, task_id: other.task_id, run_id: null, title: other.title, body: null, approval_id: null, priority: 1, created_at: 1, resolved_at: null };
  act(() => app.session.state.set((s) => ({ ...s, attention: { done: item } })));
  expect(screen.getByText(/waiting in other projects/).textContent).toMatch(/^\d+ waiting/);
});
