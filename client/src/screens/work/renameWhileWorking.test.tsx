// SPDX-License-Identifier: Apache-2.0
// Every agent step changes a task on the computer, so a rename is fenced by
// the title it replaces: it goes through while the agent works, and a rename
// from a title someone changed since is refused with the current title.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { useState } from "react";
import { RenameSheet } from "./RenameTask";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

/** Rename, as a task's More sheet in Talk opens it. */
function Rename({ taskId }: { taskId: string }) {
  const [open, setOpen] = useState(true);
  return open ? <RenameSheet task={app.session.state.get().tasks[taskId]!} onClose={() => setOpen(false)} /> : null;
}
function openRename() {
  render(<AppProvider app={app}><Rename taskId={ids.tDash} /></AppProvider>);
}
/** As the computer records an agent step: the task's revision moves on. */
function agentStep() {
  const host = window.__sim!.host;
  host.tasks = host.tasks.map((t) => t.task_id === ids.tDash ? { ...t, updated_at: Date.now(), revision: t.revision + 1 } : t);
  act(() => host.emit({ type: "task_upserted", task: host.tasks.find((t) => t.task_id === ids.tDash)! }));
}
const save = () => screen.getByRole<HTMLButtonElement>("button", { name: "Save title" });

it("renames a task while its agent keeps working", async () => {
  const task = app.session.state.get().tasks[ids.tDash]!;
  expect(task.state).toBe("working");
  openRename();
  agentStep();
  // Nothing is fenced by the agent's steps.
  expect(screen.queryByText(/changed on the computer/)).toBeNull();
  fireEvent.change(screen.getByLabelText("Task title"), { target: { value: "Check the latency chart on staging" } });
  agentStep(); agentStep(); agentStep();
  expect(screen.queryByText(/changed on the computer/)).toBeNull();
  expect(save().disabled).toBe(false);
  fireEvent.click(save());
  agentStep();
  await advance(1000);
  expect(app.session.state.get().tasks[ids.tDash]!.title).toBe("Check the latency chart on staging");
  expect(screen.queryByLabelText("Task title")).toBeNull();
});

it("refuses a rename from a title that changed since, and says what it is now", async () => {
  const task = app.session.state.get().tasks[ids.tDash]!;
  expect(task.title).not.toBe("Staging latency chart");
  openRename();
  fireEvent.change(screen.getByLabelText("Task title"), { target: { value: "Check the latency chart on staging" } });
  // Renamed on the computer before this phone hears of it.
  const host = window.__sim!.host;
  host.tasks = host.tasks.map((t) => t.task_id === ids.tDash ? { ...t, title: "Staging latency chart" } : t);
  fireEvent.click(save());
  await advance(1000);
  expect(screen.getByText(/Not renamed: this task was renamed to “Staging latency chart” in the meantime\./)).toBeTruthy();
  expect(screen.getByLabelText<HTMLInputElement>("Task title").value).toBe("Check the latency chart on staging");
  // Once the phone hears of it, it says so before another attempt.
  const renamed = app.session.state.get().tasks[ids.tDash]!;
  act(() => window.__sim!.host.emit({ type: "task_upserted", task: { ...renamed, title: "Staging latency chart", revision: renamed.revision + 1 } }));
  expect(screen.getByText(/This task is now called “Staging latency chart” on the computer/)).toBeTruthy();
  expect(save().disabled).toBe(true);
});
