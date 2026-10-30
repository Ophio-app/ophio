// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { openAttention } from "../../state/projection";
import { projectCounts } from "../../shell/ProjectSheet";
import { RouteProvider } from "../../shell/Screen";
import { projectAttention } from "./attention";
import Attention from "./Attention";
import { AttentionList } from "./shared";

let app: App;
const tick = () => act(async () => { await vi.advanceTimersByTimeAsync(300); });
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
  const host = window.__sim!.host;
  for (const attention of host.attention) { attention.resolved_at = Date.now(); host.emit({ type: "attention_resolved", attention_id: attention.attention_id }); }
  for (const projectId of [ids.pTide, ids.pNotes]) {
    const project = host.projects.find(p => p.project_id === projectId)!;
    project.active_writer = null; host.emit({ type: "project_upserted", project });
    const queue = host.talk.queue(projectId); queue.items = []; queue.writer = null; queue.paused = false; host.talk.put(queue);
    for (const suffix of ["first", "second"]) {
      const task = { ...host.tasks.find(t => t.task_id === ids.tReadme)!, project_id: projectId, task_id: `${projectId}-${suffix}`, title: `Review ${suffix}`, attention_count: 0 };
      host.tasks.push(task); host.emit({ type: "task_upserted", task });
      host.talk.enqueue(task, task.title, null, { mode: "queue", position: null, authorization: "ask_before_start" });
    }
  }
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
it("projects the computer's queue attention into one entry and badge per project, then opens the exact Review", async () => {
  const state = app.session.state.get();
  const entries = openAttention(state).filter(a => a.task_id?.endsWith("first"));
  expect(entries).toHaveLength(2);
  for (const projectId of [ids.pTide, ids.pNotes]) {
    expect(projectAttention(state).find(a => a.project.project_id === projectId)?.count).toBe(1);
    expect(projectCounts(state)[projectId]?.attention).toBe(1);
  }
  app.ui.selectTab("todo"); app.ui.setTodoAll(true);
  const list = render(<AppProvider app={app}><AttentionList projectId={ids.pNotes} /></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: /Review first/ }));
  const route = app.ui.route(); expect(route.screen).toBe("attention"); list.unmount();
  if (route.screen !== "attention") throw Error("Expected attention");
  render(<AppProvider app={app}><RouteProvider value={{ tab: app.ui.get().tab, route, depth: 1, active: true }}><Attention route={route} /></RouteProvider></AppProvider>);
  await tick();
  expect(app.ui.route()).toMatchObject({ screen: "review", projectId: ids.pNotes, taskId: `${ids.pNotes}-first`, queueItemId: window.__sim!.host.talk.queue(ids.pNotes).items[0]!.item_id });
  expect(app.ui.get()).toMatchObject({ projectId: ids.pNotes, todoAll: true });
});
it.each(["queue_pause", "queue_remove", "queue_move", "queue_start"] as const)("clears the former front attention on %s without affecting the other project", async command => {
  const host = window.__sim!.host;
  const queue = host.talk.queue(ids.pNotes); const item = queue.items[0]!;
  const body = command === "queue_pause" ? { command, project_id: ids.pNotes }
    : command === "queue_move" ? { command, item_id: item.item_id, position: 1 }
    : command === "queue_start" ? { command, item_id: item.item_id, task_revision: host.tasks.find(t => t.task_id === item.task_id)!.revision }
    : { command, item_id: item.item_id };
  await act(async () => { await host.command({ command_id: crypto.randomUUID(), issued_at: Date.now(), expires_at: null, expected_revision: queue.revision, body }); });
  const entries = openAttention(app.session.state.get());
  expect(entries.some(a => a.task_id === item.task_id)).toBe(false);
  expect(entries.some(a => a.task_id === `${ids.pTide}-first`)).toBe(true);
  // A started task holds the writer, so the next go-ahead waits for it.
  expect(entries.filter(a => a.task_id?.startsWith(ids.pNotes))).toHaveLength(command === "queue_pause" || command === "queue_start" ? 0 : 1);
});
