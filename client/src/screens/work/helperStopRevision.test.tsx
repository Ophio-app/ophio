// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { workIds } from "../../dev/sim/work";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import type { CommandEnvelope } from "../../protocol/types";
import { AgentChildren } from "./AgentChildren";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "agents" }, depth: 1, active: true }}>{children}</RouteProvider></AppProvider>;
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("Stop helper still reaches the helper when the working agent takes a step while Stop is in flight", async () => {
  const host = window.__sim!.host;
  // A working task with running helpers; stop_child is admitted only at the
  // task's current revision, and every agent step moves that revision.
  act(() => host.children.background(false));
  const task = app.session.state.get().tasks[workIds.receipt]!;
  const original = host.command.bind(host);
  let raced = false;
  vi.spyOn(host, "command").mockImplementation(async (env: CommandEnvelope) => {
    if (env.body.command === "stop_child" && !raced) {
      raced = true;
      const hostTask = host.tasks.find(t => t.task_id === task.task_id)!;
      hostTask.revision += 1;
      host.emit({ type: "task_upserted", task: structuredClone(hostTask) });
    }
    return original(env);
  });
  render(<AgentChildren task={task} />, { wrapper });
  fireEvent.click(screen.getByRole("button", { name: "Stop helper · Build preview" }));
  await advance(1500);
  expect(host.runs.find(r => r.run_id === workIds.run)?.children[0]?.state).toBe("stopped");
  expect(screen.queryByText(/This task changed/)).toBeNull();
});
