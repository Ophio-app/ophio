// SPDX-License-Identifier: Apache-2.0
import { Profiler } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { Composer } from "./Composer";

let app: App;
const commits: string[] = [];
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
  window.__sim!.host.dispose();
});

async function mount(kind: "stopped chat" | "new task" | "steer") {
  const { session } = app;
  const state = session.state.get();
  const project = state.projects[ids.pTide]!;
  const task = kind === "new task" ? null : { ...state.tasks[ids.tHeaders]!, conversation: kind === "stopped chat", mode: kind === "stopped chat" ? "ask" as const : "work" as const, state: kind === "steer" ? "working" as const : "interrupted" as const };
  const run = task ? { ...state.runs[task.current_run!]!, state: kind === "steer" ? "working" as const : "interrupted" as const } : null;
  const harness = state.harnesses.find(h => h.kind === (task?.harness ?? "claude_code"))!;
  session.state.set({ ...state, tasks: { ...state.tasks, ...(task ? { [task.task_id]: task } : {}) } });
  app.ui.selectTask(project.project_id, task?.task_id ?? null);
  app.ui.saveDraft(project.project_id, task?.task_id ?? null, { text: "Please " });
  const tree = <AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}>
    <Profiler id="composer" onRender={(_id, phase) => commits.push(phase)}>
      <Composer project={project} task={task} run={run} harness={harness} agent={task ? null : { harness: harness.kind, model: null, effort: null }} />
    </Profiler>
  </RouteProvider></AppProvider>;
  const mounted = render(tree);
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  commits.length = 0;
  return { mounted, tree, taskId: task?.task_id ?? null };
}
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it.each(["stopped chat", "new task", "steer"] as const)("saves each %s edit without scheduling a second update from an effect", async kind => {
  const { taskId, mounted, tree } = await mount(kind);
  const field = screen.getByRole<HTMLTextAreaElement>("textbox");
  // The first edit also settles the newly mounted subscribers. Measure the
  // continuous edits after it, not that one-time transition into typing.
  fireEvent.change(field, { target: { value: "Please p" } });
  let text = field.value;
  for (const letter of "ut the table beside the window and keep every letter.") {
    commits.length = 0;
    text += letter;
    fireEvent.change(field, { target: { value: text } });
    expect(field.value).toBe(text);
    expect(app.ui.draft(ids.pTide, taskId)?.text).toBe(text);
    // A passive reset on every edit keeps another update pending. A burst
    // of browser input can hit React's nested-update limit and lose an edit.
    expect(commits).toEqual(["update"]);
  }
  mounted.unmount();
  render(tree);
  expect(screen.getByRole<HTMLTextAreaElement>("textbox").value).toBe(text);
  act(() => { window.dispatchEvent(new Event("pagehide")); });
  expect(new UiStore(app.profile.profileId).draft(ids.pTide, taskId)?.text).toBe(text);
});
