// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import type { Task } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { stopStore } from "../control/stops";
import { openMayDo } from "./MayDoLine";
import Talk from "./Talk";

let app: App;
const tick = (ms = 1500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });

/** Talk on tidewater-api, after `change` has had its way with the computer's tasks. */
async function setup(change: (tasks: Task[]) => void = () => {}) {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  change(window.__sim!.host.tasks);
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  app.ui.selectProject(ids.pTide);
  await tick(100);
  const route = { screen: "talk" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /></RouteProvider></AppProvider>);
  await tick(100);
}
afterEach(() => { cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const line = () => document.querySelector(".chat-head__sub .may-do") as HTMLButtonElement | null;

async function startConversation() {
  fireEvent.click(screen.getByRole("button", { name: "New conversation" }));
  await tick(100);
  fireEvent.change(screen.getAllByRole("textbox").at(-1)!, { target: { value: "How does the rate limiter decide when to back off?" } });
  fireEvent.click(screen.getByRole("button", { name: "Start conversation" }));
  await tick(4000);
}

it("says a chat is a chat and what its access lets it do, never Ask, and opens the access bar", async () => {
  await setup();
  await startConversation();
  expect(line()?.textContent).toBe("Chat, can read tidewater-api and use the internet");
  expect(line()?.getAttribute("aria-label")).toBe("Chat, can read tidewater-api and use the internet. Change what it may reach");
  expect(document.querySelector(".chat-head")?.textContent).not.toMatch(/\bAsk\b/);
  fireEvent.click(line()!);
  await tick(50);
  const bar = screen.getByRole("group", { name: "What the agent may reach" });
  expect(bar.classList.contains("is-called")).toBe(true);
  expect(document.activeElement?.getAttribute("aria-label")).toMatch(/^Read: /);
  await tick(2000);
  expect(bar.classList.contains("is-called")).toBe(false);
  // Turning the internet off is said at once.
  fireEvent.click(within(bar).getByRole("button", { name: "Internet" }));
  await tick(1500);
  expect(line()?.textContent).toBe("Chat, can read tidewater-api");
});

it("says a Work task's mode and opens a sheet with it", async () => {
  await setup();
  act(() => app.ui.selectTask(ids.pTide, ids.tDash));
  await tick(100);
  expect(line()?.textContent).toBe("Task in Work mode, can change tidewater-api");
  expect(screen.queryByText(/read-only/)).toBeNull();
  fireEvent.click(line()!);
  await tick(50);
  const sheet = screen.getByRole("dialog", { name: "What this task may do" });
  expect((within(sheet).getByRole("radio", { name: "Work" }) as HTMLInputElement).checked).toBe(true);
  expect((within(sheet).getByRole("radio", { name: /^Plan/ }) as HTMLInputElement).disabled).toBe(true);
  expect(sheet.textContent).toContain("A task keeps the mode it started in.");
  expect(sheet.textContent).toContain("It can change files and run commands in tidewater-api");
});

it("says a Plan task changes nothing", async () => {
  await setup((tasks) => { tasks.find((t) => t.task_id === ids.tDash)!.mode = "plan"; });
  act(() => app.ui.selectTask(ids.pTide, ids.tDash));
  await tick(100);
  expect(line()?.textContent).toBe("Task in Plan mode, changes nothing");
  fireEvent.click(line()!);
  await tick(50);
  const sheet = screen.getByRole("dialog", { name: "What this task may do" });
  expect((within(sheet).getByRole("radio", { name: "Plan" }) as HTMLInputElement).checked).toBe(true);
  expect(sheet.textContent).toContain("It only reads.");
});

it("opens a task and its controls from elsewhere", async () => {
  await setup();
  act(() => app.ui.showTaskList());
  await tick(100);
  expect(line()).toBeNull();
  const task = app.session.state.get().tasks[ids.tDash]!;
  act(() => openMayDo(app.ui, task));
  await tick(500);
  expect(line()?.textContent).toBe("Task in Work mode, can change tidewater-api");
  expect(screen.getByRole("dialog", { name: "What this task may do" })).toBeTruthy();
});
