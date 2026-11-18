// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession, NotConnectedError } from "../../host/session";
import type { CommandReceipt } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { Composer } from "./Composer";

let app: App;
const tick = () => act(async () => { await vi.advanceTimersByTimeAsync(0); });
const receipt: CommandReceipt = { command_id: "accepted", status: "accepted", seq: null, result: null, error: null };

beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
  window.__sim!.host.dispose();
  const state = session.state.get();
  const project = state.projects[ids.pTide]!;
  const task = { ...state.tasks[ids.tHeaders]!, conversation: true, mode: "ask" as const, state: "interrupted" as const };
  session.state.set({ ...state, tasks: { ...state.tasks, [task.task_id]: task } });
  app.ui.selectTask(project.project_id, task.task_id);
  app.ui.saveDraft(project.project_id, task.task_id, { text: "Please continue." });
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}>
    <Composer project={project} task={task} run={state.runs[task.current_run!] ?? null} harness={state.harnesses.find(h => h.kind === task.harness)!} agent={null} />
  </RouteProvider></AppProvider>);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

function doubleTapSend() {
  const button = screen.getByRole<HTMLButtonElement>("button", { name: "Send" });
  expect(button.disabled).toBe(false);
  // Both taps reach the same rendered handler before React updates the button.
  act(() => { button.click(); button.click(); });
}

it("sends a stopped Codex conversation once for two fast taps, then allows later text", async () => {
  let accept!: (value: CommandReceipt) => void;
  const pending = new Promise<CommandReceipt>(resolve => { accept = resolve; });
  const command = vi.spyOn(app.session, "command").mockResolvedValue(receipt).mockReturnValueOnce(pending);
  doubleTapSend();
  await tick();
  expect(command).toHaveBeenCalledTimes(1);
  expect(command.mock.calls[0]![0]).toMatchObject({ command: "resume_task", task_id: ids.tHeaders, text: "Please continue." });

  fireEvent.change(screen.getByRole("textbox"), { target: { value: "A later message." } });
  await act(async () => { accept(receipt); });
  expect(screen.queryByText("Not sent")).toBeNull();
  expect(app.ui.draft(ids.pTide, ids.tHeaders)).toMatchObject({ text: "A later message.", attempt: null });

  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await tick();
  expect(command).toHaveBeenCalledTimes(2);
  expect(command.mock.calls[1]![0]).toMatchObject({ command: "resume_task", text: "A later message." });
  expect(app.ui.draft(ids.pTide, ids.tHeaders)).toBeNull();
});

it("releases the fast-tap guard after failure so the kept text can be retried", async () => {
  const command = vi.spyOn(app.session, "command").mockResolvedValue(receipt).mockRejectedValueOnce(new NotConnectedError());
  doubleTapSend();
  await tick();
  expect(command).toHaveBeenCalledTimes(1);
  expect(app.ui.draft(ids.pTide, ids.tHeaders)).toMatchObject({ text: "Please continue.", attempt: { phase: "not_delivered" } });

  fireEvent.click(screen.getByRole("button", { name: "Send again" }));
  await tick();
  expect(command).toHaveBeenCalledTimes(2);
  expect(app.ui.draft(ids.pTide, ids.tHeaders)).toBeNull();
});
