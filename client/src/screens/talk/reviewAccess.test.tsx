// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import type { ConversationAccess, Task } from "../../protocol/types";
import Review from "./Review";

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
const tick = async (ms = 500) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

/** A finished conversation with `access`, and its reply in Review. */
async function reply(access: ConversationAccess | undefined) {
  const host = window.__sim!.host;
  const base = host.tasks.find(t => t.task_id === ids.tHeaders)!;
  const task: Task = { ...base, mode: "ask", conversation: true, state: "completed", ...(access ? { access } : {}) };
  host.tasks = host.tasks.map(t => t.task_id === task.task_id ? task : t);
  host.emit({ type: "task_upserted", task });
  app.ui.saveDraft(task.project_id, task.task_id, { text: "Go on with the headers." });
  await tick();
  const route: Route = { screen: "review", projectId: task.project_id, taskId: task.task_id, intent: "continue" };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}><Review route={route} /></RouteProvider></AppProvider>);
  await tick();
  return document.querySelector(".review")?.textContent ?? document.body.textContent ?? "";
}

it("tells a conversation that may change files so, never that it stays read-only", async () => {
  const pid = window.__sim!.host.tasks.find(t => t.task_id === ids.tHeaders)!.project_id;
  const text = await reply({ read: [], write: [pid], whole_read: false, whole_write: false, internet: true, local_network: false });
  expect(screen.getByText("What it may reach.")).toBeTruthy();
  expect(text).toContain("The agent can change");
  expect(text).toContain("may change files where you’ve let it");
  expect(text).not.toMatch(/read-only|changes nothing/);
});

it("tells a conversation that only reads so", async () => {
  const text = await reply(undefined);
  expect(text).toContain("can’t change anything");
  expect(text).toContain("reads but changes nothing");
  expect(text).not.toContain("start a new task in Work mode");
});
