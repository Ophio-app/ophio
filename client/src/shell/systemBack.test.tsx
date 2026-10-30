// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { HostSession } from "../host/session";
import { ids } from "../dev/sim/scenario";
import { runningTasks } from "../state/projection";
import { Shell } from "./Shell";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  app.ui.selectProject(ids.pTide);
  // Someone back on Chat, not a first run, which opens on what needs them.
  app.ui.openFirst(false);
  session.start(); await vi.advanceTimersByTimeAsync(900);
  render(<AppProvider app={app}><Shell /></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(500); });
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

/** Android's Back, as the shell asks it; false means the app is left. */
const systemBack = () => {
  let handled = false;
  act(() => { handled = window.systemBack!(); });
  return handled;
};
const rail = () => within(document.querySelector<HTMLElement>(".app__safety")!);
const chatShown = () => vi.waitFor(async () => {
  await act(async () => { await vi.dynamicImportSettled(); });
  expect(document.querySelector(".chat-pins")).not.toBeNull();
}, { timeout: 10_000 });
/** The running dashboard check, as a conversation, which Talk opens on (D-065). */
const conversing = () => act(() => {
  const host = window.__sim!.host;
  const dash = { ...host.tasks.find((t) => t.task_id === ids.tDash)!, conversation: true };
  host.tasks = host.tasks.map((t) => (t.task_id === dash.task_id ? dash : t));
  host.emit({ type: "task_upserted", task: dash });
});

it("Back collapses the Safety panel Chat's activity row opened, then shows the project's conversations, and only Back there leaves the app", async () => {
  conversing();
  await chatShown();
  // Talk sums the rail up in a row under its header (D-054).
  expect(rail().queryByRole("button", { name: /^Safety ·/ })).toBeNull();
  fireEvent.click(within(document.querySelector<HTMLElement>(".app__main")!).getByRole("button", { name: /Show Stop and input controls/ }));
  expect(rail().getByRole("button", { name: /^Safety ·/ }).getAttribute("aria-expanded")).toBe("true");
  // Not a modal: the page behind stays in the accessibility tree.
  expect(document.querySelector(".app__main")!.closest("[inert], [aria-hidden='true']")).toBeNull();
  expect(systemBack()).toBe(true);
  expect(rail().queryByRole("button", { name: /^Safety ·/ })).toBeNull();
  expect(app.ui.get().tab).toBe("talk");
  expect(document.querySelector(".nav")).toBeNull();
  // As a chat's Back goes to its chats, with the tabs up and the rail back.
  expect(app.ui.canGoBack()).toBe(true);
  expect(systemBack()).toBe(true);
  expect(app.ui.get()).toMatchObject({ tab: "talk", taskList: true });
  expect(document.querySelector(".talk-tasks")).not.toBeNull();
  expect(document.querySelector(".nav")).not.toBeNull();
  expect(rail().getByRole("button", { name: /^Safety ·/ })).toBeTruthy();
  expect(systemBack()).toBe(false);
});

it("the chat header's back arrow shows the project's conversations with all five tabs, and Chat's tab keeps them", async () => {
  conversing();
  await chatShown();
  expect(document.querySelector(".nav")).toBeNull();
  fireEvent.click(document.querySelector<HTMLElement>(".chat-head__back")!);
  const nav = within(document.querySelector<HTMLElement>(".nav")!);
  for (const tab of ["Chat", "Control", "Tasks", "Library", "To-Do"]) expect(nav.getByRole("button", { name: new RegExp(`^${tab}`) })).toBeTruthy();
  fireEvent.click(nav.getByRole("button", { name: /^Tasks/ }));
  expect(app.ui.get().tab).toBe("work");
  fireEvent.click(within(document.querySelector<HTMLElement>(".nav")!).getByRole("button", { name: /^Chat/ }));
  expect(document.querySelector(".talk-tasks")).not.toBeNull();
  expect(document.querySelector(".nav")).not.toBeNull();
});

it("Back from a task opened in Chat goes to Tasks's tasks, as its back arrow does", async () => {
  act(() => app.ui.selectTask(ids.pTide, ids.tDash));
  await chatShown();
  expect(systemBack()).toBe(true);
  expect(app.ui.get()).toMatchObject({ tab: "work", taskList: true });
  expect(systemBack()).toBe(true);
  expect(app.ui.get().tab).toBe("talk");
  // Talk's root is its conversations: Back there leaves the app.
  expect(document.querySelector(".talk-tasks")).not.toBeNull();
  expect(systemBack()).toBe(false);
});

it("Back goes from a screen to its tab's root, then from another tab to Chat", () => {
  act(() => app.ui.selectTab("control"));
  act(() => app.ui.push({ screen: "connections" }));
  expect(systemBack()).toBe(true);
  expect(app.ui.get().tab).toBe("control");
  expect(app.ui.route()).toEqual({ screen: "control" });
  expect(systemBack()).toBe(true);
  expect(app.ui.get().tab).toBe("talk");
  // No conversation is going, so Talk shows its conversations: the root.
  expect(systemBack()).toBe(false);
});

it("the expanded Safety panel opens a running task beside its Stop", async () => {
  act(() => app.ui.selectTab("control"));
  fireEvent.click(rail().getByRole("button", { name: /^Safety ·/ }));
  const task = runningTasks(app.session.state.get())[0]!;
  const stop = rail().getByRole("button", { name: new RegExp(`^Stop · ${task.title}`) });
  const open = rail().getByRole("button", { name: `Open task ${task.title}` });
  expect(stop.closest(".safety-rail__task")).toBe(open.closest(".safety-rail__task"));
  fireEvent.click(open);
  expect(app.ui.get().tab).toBe("talk");
  expect(app.ui.get().taskByProject[task.project_id]).toBe(task.task_id);
  await chatShown();
  // Collapsed, and summed up again under Talk's header.
  expect(rail().queryByRole("button", { name: /^Safety ·/ })).toBeNull();
});

it("keeps the rail while Chat has no chat header to sum it up in, as while a new task arrives", async () => {
  conversing();
  await chatShown();
  expect(rail().queryByRole("button", { name: /^Safety ·/ })).toBeNull();
  act(() => app.ui.selectCreated(app.ui.get().projectId!, "not-yet-here"));
  expect(document.querySelector(".chat-pins")).toBeNull();
  expect(rail().getByRole("button", { name: /^Safety ·/ })).toBeTruthy();
  expect(document.querySelector(".nav")).not.toBeNull();
});
