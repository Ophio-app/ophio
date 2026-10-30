// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { RouteProvider } from "../../shell/Screen";
import { RailOpen } from "../../shell/focus";
import { UrgentSlot } from "../../design/urgent";
import { Store } from "../../state/store";
import { openAttention } from "../../state/projection";
import SafetyStrip from "../control/SafetyStrip";
import { stopStore } from "../control/stops";
import Talk from "./Talk";

let app: App;
const tick = (ms = 1500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  app.ui.selectProject(ids.pTide);
  await tick(100);
});
afterEach(() => { cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

// As the shell mounts them while a task is open.
function talk(taskId = ids.tDash) {
  act(() => app.ui.selectTask(ids.pTide, taskId));
  const route = { screen: "talk" as const };
  const rail = new Store(false);
  render(<AppProvider app={app}><RailOpen.Provider value={rail}><UrgentSlot.Provider value={SafetyStrip}>
    <RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /></RouteProvider>
    <SafetyStrip open={rail} except={{ taskId, keepDisplay: true }} stow />
  </UrgentSlot.Provider></RailOpen.Provider></AppProvider>);
}
const stops = (title: RegExp) => screen.queryAllByRole("button", { name: new RegExp(`^Stop · ${title.source}`) });

it("keeps this task's Stop by its name, once, and every other running task's Stop one row away (D-054)", async () => {
  talk();
  await tick();
  expect(stops(/Check the staging/)).toHaveLength(1);
  expect(stops(/Fix the flaky/)).toHaveLength(0);
  expect(screen.queryByRole("button", { name: "Settings" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /Also running · Fix the flaky.*Show Stop and input controls/ }));
  await tick(100);
  expect(stops(/Fix the flaky/)).toHaveLength(1);
  expect(stops(/Check the staging/)).toHaveLength(1);
});

it("keeps another task's display off a task that uses none, still one row from its Stop", async () => {
  app.ui.selectTask(ids.pTide, ids.tFlaky);
  talk(ids.tFlaky);
  await tick();
  const row = screen.getByRole("button", { name: /Show Stop and input controls/ });
  expect(row.textContent).toBe("Also running · Check the staging dashboard renders the new latency chart");
  fireEvent.click(row);
  await tick(100);
  expect(stops(/Check the staging/)).toHaveLength(1);
});

it("goes back from a task to Tasks, counting what needs the person, and the name is only a name (D-058, D-065)", async () => {
  talk();
  await tick();
  const all = openAttention(app.session.state.get()).length;
  expect(all).toBeGreaterThan(1);
  const back = screen.getByRole("button", { name: `Tasks, ${all} need attention` });
  expect(screen.getByRole("heading", { level: 1 }).querySelector("button")).toBeNull();
  expect(app.ui.canGoBack()).toBe(true);
  fireEvent.click(back);
  await tick(100);
  // Tasks are Work's; Talk shows its conversations when it is next chosen.
  expect(app.ui.get()).toMatchObject({ tab: "work", taskList: true });
});

it("goes back from a conversation to Chat's conversations, as a messaging app's chats (D-065)", async () => {
  const host = window.__sim!.host;
  const asked = { ...host.tasks.find(t => t.task_id === ids.tHeaders)!, conversation: true };
  host.tasks = host.tasks.map(t => t.task_id === asked.task_id ? asked : t);
  host.emit({ type: "task_upserted", task: asked });
  await tick(100);
  talk(ids.tHeaders);
  await tick();
  // The message box says which model it talks with; the header doesn't repeat it.
  expect(document.querySelector(".chat-head__model")).toBeNull();
  expect(document.querySelector(".chat-head")?.textContent).not.toMatch(/GPT/);
  fireEvent.click(screen.getByRole("button", { name: /^Conversations/ }));
  await tick(100);
  // The project's name heads them and opens the others, and it is the last step back.
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.getByRole("heading", { level: 1 }).querySelector("button")?.textContent).toMatch(/^tidewater-api/);
  expect(screen.getByRole("button", { name: /^New conversation/ })).toBeTruthy();
  expect(screen.getByRole("button", { name: /^Go to Tasks/ })).toBeTruthy();
  expect(app.ui.get()).toMatchObject({ tab: "talk", taskList: true });
  expect(app.ui.canGoBack()).toBe(false);
  fireEvent.click(within(document.querySelector<HTMLElement>(".talk-tasks")!).getByRole("button", { name: /^Add rate-limit headers/ }));
  await tick(100);
  expect(app.ui.get()).toMatchObject({ taskList: false, taskByProject: { [ids.pTide]: ids.tHeaders } });
  expect(screen.getByRole("heading", { level: 1 }).textContent).toMatch(/Add rate-limit headers/);
});

it("the task's ⋯ carries its pictures, instructions and record, which the chat header has no room for", async () => {
  talk();
  await tick();
  fireEvent.click(screen.getByRole("button", { name: "More about this task" }));
  await tick(100);
  const more = screen.getByRole("dialog", { name: "This task" });
  for (const name of [/^Pictures and videos/, /^Instructions/, /^What happened in this task/])
    expect(within(more).getByRole("button", { name })).toBeTruthy();
});
