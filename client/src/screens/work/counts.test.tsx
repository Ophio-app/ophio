// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import { BottomNav } from "../../shell/BottomNav";
import { RouteProvider } from "../../shell/Screen";
import { openAttention } from "../../state/projection";
import { TaskRow } from "./shared";
import Work from "./Work";

// Leaves only the first few open items, so a count can be exactly one.
const shown = vi.hoisted(() => ({ limit: null as number | null }));
vi.mock("../../state/projection", async importOriginal => {
  const real = await importOriginal<typeof import("../../state/projection")>();
  return { ...real, openAttention: (s: Parameters<typeof real.openAttention>[0]) => real.openAttention(s).slice(0, shown.limit ?? undefined) };
});

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear(); shown.limit = null;
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing fixture profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  document.getElementById("u57-blocks")?.remove();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("says one item needs attention, and says the Tasks tab's count as one name", async () => {
  // Chrome lays out an absolutely positioned span, such as a visually hidden
  // one, as a block, and puts a space around a block when it reads a name.
  const blocks = document.createElement("style");
  blocks.id = "u57-blocks";
  blocks.textContent = ".visually-hidden { display: block; }";
  document.head.append(blocks);

  const all = openAttention(app.session.state.get()).length;
  expect(all).toBeGreaterThan(1);
  shown.limit = 1;
  const state = app.session.state.get();
  const item = openAttention(state)[0]!;
  const task = state.tasks[item.task_id!]!;
  const project = state.projects[task.project_id]!;
  app.ui.selectProject(project.project_id);

  const route = { screen: "work" } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><BottomNav /><TaskRow task={task} /></RouteProvider></AppProvider>);
  await advance();
  expect(screen.getByRole("button", { name: "Tasks, 1 needs attention" })).toBeTruthy();
  expect(screen.getByText("1 needs attention")).toBeTruthy();
  cleanup();

  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><Work route={route} /></RouteProvider></AppProvider>);
  await advance();
  expect(screen.getByRole("heading", { name: "Needs you" })).toBeTruthy();
  expect(document.querySelectorAll(".work-lead")).toHaveLength(1);
  fireEvent.click(within(screen.getByRole("heading", { level: 1 })).getByRole("button"));
  expect(screen.getByRole("button", { name: new RegExp(`${project.name}.*1 needs you`) })).toBeTruthy();
  cleanup();

  shown.limit = null;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><BottomNav /></RouteProvider></AppProvider>);
  await advance();
  expect(screen.getByRole("button", { name: `Tasks, ${all} need attention` })).toBeTruthy();
});
