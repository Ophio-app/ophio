// SPDX-License-Identifier: Apache-2.0
// Archiving doesn't wait for the computer: the tasks leave their list the
// moment the person asks, are all sent together, and any the computer
// refuses come back with why. Undo brings them back the same way; a
// computer that can't bring them back asks first instead.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { Toasts } from "../../design/Toast";
import { CommandError, HostSession } from "../../host/session";
import { uuidv7 } from "../../protocol/ids";
import type { Command, ProtocolError } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { UNARCHIVE_FEATURE } from "./archive";
import { hasEndedTask } from "./logic";
import Work from "./Work";

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
  app.ui.selectProject(ids.pTide); app.ui.selectTab("work");
  // Enough finished tasks that one at a time would show.
  const host = window.__sim!.host;
  const base = host.tasks.find((t) => t.task_id === ids.tHeaders)!;
  act(() => {
    for (let n = 0; n < 6; n++) {
      const task = { ...base, task_id: uuidv7(Date.now() + n), title: `Finished ${n}`, current_run: null, updated_at: Date.now() - 9000 - n };
      host.tasks.push(task); host.emit({ type: "task_upserted", task });
    }
  });
  await advance();
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

const finished = () => Object.values(app.session.state.base().tasks).filter((t) => t.project_id === ids.pTide && !t.conversation && hasEndedTask(t) && t.state !== "archived");
function show() {
  const route = { screen: "work" } as const;
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><Work route={route} /><Toasts /></RouteProvider></AppProvider>);
}
/** Archive all in Read: at once, with Undo. */
function archiveAllRead() {
  fireEvent.click(screen.getByRole("radio", { name: /^Finished \d+/ }));
  const read = screen.getByRole("heading", { name: "Read" }).closest("section")!;
  fireEvent.click(within(read).getByRole("button", { name: /^Archive all \d+ read/ }));
}
const undo = () => fireEvent.click(within(screen.getByText(/^Archived \d+ tasks/).closest(".toast")! as HTMLElement).getByRole("button", { name: "Undo" }));
const archives = (spy: { mock: { calls: unknown[][] } }) =>
  spy.mock.calls.map((c) => c[0] as Command).filter((c) => c.command === "archive_task");

it("takes every task off the list at once and sends them all together", async () => {
  const before = finished();
  show(); await advance();
  const send = vi.spyOn(app.session, "command");
  archiveAllRead();
  const asked = archives(send).map((c) => "task_id" in c && c.task_id);
  // Every one is sent before the computer has answered any.
  expect(asked.length).toBeGreaterThan(5);
  for (const id of asked) expect(app.session.state.get().tasks[id as string]?.state).toBe("archived");
  expect(screen.getByText(`Archived ${asked.length} tasks, now in Tasks › Archived`)).toBeTruthy();
  expect(screen.queryByRole("heading", { name: "Read" })).toBeNull();
  await advance(1500);
  for (const id of asked) expect(app.session.state.base().tasks[id as string]?.state).toBe("archived");
  expect(finished().length).toBe(before.length - asked.length);
});

it("puts a task the computer refuses back, and says why", async () => {
  show(); await advance();
  const real = app.session.command.bind(app.session);
  let refused: string | null = null;
  vi.spyOn(app.session, "command").mockImplementation((body, opts) => {
    if (body.command !== "archive_task" || refused) return real(body, opts);
    refused = body.task_id;
    const error = { code: "conflict", message: "It started again." } as ProtocolError;
    return new Promise((_, no) => setTimeout(() => no(new CommandError(error, null)), 300));
  });
  archiveAllRead();
  expect(app.session.state.get().tasks[refused!]?.state).toBe("archived");
  await advance(1500);
  expect(app.session.state.get().tasks[refused!]?.state).not.toBe("archived");
  const toast = screen.getByText(/^1 task not archived and back in the list\. .*It started again/);
  expect(toast.closest(".toast")!.className).toContain("toast--stop");
  const read = screen.getByRole("heading", { name: "Read" }).closest("section")!;
  expect(within(read).getByText(app.session.state.get().tasks[refused!]!.title)).toBeTruthy();
});

it("brings every task back at once with Undo, and on the computer too", async () => {
  const before = finished().map((t) => [t.task_id, t.state] as const);
  show(); await advance();
  const send = vi.spyOn(app.session, "command");
  archiveAllRead();
  expect(screen.queryByRole("dialog")).toBeNull();
  undo();
  // Back in the list before the computer has answered either way.
  for (const [id, state] of before) expect(app.session.state.get().tasks[id]?.state).toBe(state);
  expect(screen.getByRole("heading", { name: "Read" })).toBeTruthy();
  await advance(1500);
  const back = send.mock.calls.map((c) => c[0] as Command).filter((c) => c.command === "unarchive_task");
  expect(back.length).toBe(archives(send).length);
  for (const [id, state] of before) expect(app.session.state.base().tasks[id]?.state).toBe(state);
  await advance(6000);
  for (const [id, state] of before) expect(app.session.state.get().tasks[id]?.state).toBe(state);
});

it("says which stay archived when the computer won't bring them back", async () => {
  show(); await advance();
  const real = app.session.command.bind(app.session);
  let kept: string | null = null;
  vi.spyOn(app.session, "command").mockImplementation((body, opts) => {
    if (body.command !== "unarchive_task" || kept) return real(body, opts);
    kept = body.task_id;
    const error = { code: "conflict", message: "This task can't be brought back; it stays in Archived to read" } as ProtocolError;
    return Promise.reject(new CommandError(error, null));
  });
  archiveAllRead();
  undo();
  await advance(1500);
  expect(app.session.state.get().tasks[kept!]?.state).toBe("archived");
  const toast = screen.getByText(/^1 task stays in Archived\. /);
  expect(toast.closest(".toast")!.className).toContain("toast--stop");
});

it("asks first where the computer can't bring an archived task back", async () => {
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter((f) => f !== UNARCHIVE_FEATURE) } })));
  show(); await advance();
  const send = vi.spyOn(app.session, "command");
  archiveAllRead();
  expect(archives(send)).toEqual([]);
  const dialog = screen.getByRole("dialog", { name: /^Archive \d+ tasks/ });
  fireEvent.click(within(dialog).getByRole("button", { name: /^Archive \d+ tasks/ }));
  expect(archives(send).length).toBeGreaterThan(5);
  expect(within(screen.getByText(/^Archived \d+ tasks/).closest(".toast")! as HTMLElement).queryByRole("button", { name: "Undo" })).toBeNull();
});
