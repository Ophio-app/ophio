// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Review from "../talk/Review";
import Receipt from "./Receipt";
import Work from "./Work";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "work" }, depth: 1, active: true }}>{children}</RouteProvider></AppProvider>;
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
  app.ui.selectTask(ids.pTide, null);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

/** The run holding tidewater-api's writer ends without a known result, as on the companion. */
async function leaveWriterUnresolved(): Promise<string> {
  const host = window.__sim!.host;
  const run = host.runs.find((r) => r.run_id === ids.rFlaky)!;
  const task = host.tasks.find((t) => t.task_id === run.task_id)!;
  const held = { ...run, state: "reconciliation_required" as const };
  const needs = { ...task, state: "reconciliation_required" as const, revision: task.revision + 1 };
  host.runs = host.runs.map((r) => (r.run_id === run.run_id ? held : r));
  host.tasks = host.tasks.map((t) => (t.task_id === task.task_id ? needs : t));
  act(() => {
    host.emit({ type: "run_upserted", run: held });
    host.emit({ type: "task_upserted", task: needs });
  });
  await advance();
  return task.title;
}

it("names the task that holds the project when nothing is running, and opens it", async () => {
  const title = await leaveWriterUnresolved();
  render(<Work route={{ screen: "work" }} />, { wrapper });
  fireEvent.click(screen.getByRole("radio", { name: /^Open/ }));
  expect(screen.queryByText("No task is running in this project.")).toBeNull();
  expect(screen.getByText(`“${title}” ended without a known result, so nothing else can change files in tidewater-api until you check what happened.`)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Check it" }));
  expect(app.ui.route()).toMatchObject({ screen: "receipt" });
});

it("says why a new Work task can't start now and links to the task that holds the project", async () => {
  const title = await leaveWriterUnresolved();
  app.ui.saveDraft(ids.pTide, null, { text: "Tidy the README" });
  render(<Review route={{ screen: "review", projectId: ids.pTide, taskId: null }} />, { wrapper });
  expect(screen.getByRole("radio", { name: "Start now" }).hasAttribute("disabled")).toBe(true);
  expect(screen.getByText(new RegExp(`^“${title}” ended without a known result`))).toBeTruthy();
  expect(screen.queryByText(/Another task is changing files/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Check it" }));
  expect(app.ui.route()).toMatchObject({ screen: "receipt" });
});

it("records what happened from the receipt Check it opens, which frees the project", async () => {
  const title = await leaveWriterUnresolved();
  const taskId = window.__sim!.host.tasks.find((t) => t.title === title)!.task_id;
  render(<Receipt route={{ screen: "receipt", taskId }} />, { wrapper });
  expect(screen.getByText("The outcome needs your decision")).toBeTruthy();
  expect(screen.getByText(/This run ended without a known result\. Look over what it did below/)).toBeTruthy();
  // What it did is on the phone, a tap away; neither answer is the default.
  expect(screen.getByRole("button", { name: /^Changed files/ })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Mark completed" }).className).toBe(screen.getByRole("button", { name: "Mark failed" }).className);
  expect(document.querySelector(".receipt-outcome")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Mark completed" }));
  await advance();
  expect(app.session.state.get().tasks[taskId]?.state).toBe("completed");
  // Released: queued work in the project may now take the writer, but not this run.
  expect(app.session.state.get().projects[ids.pTide]?.active_writer).not.toBe(ids.rFlaky);
  expect(screen.queryByText("The outcome needs your decision")).toBeNull();
  expect(screen.getByText("Marked completed. The computer hasn’t verified it.")).toBeTruthy();
  await advance(2000);
  // Then what it came to, and the way to archive it.
  expect(screen.getByRole("region", { name: "Done" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Archive" })).toBeTruthy();
  cleanup();
  render(<Work route={{ screen: "work" }} />, { wrapper });
  expect(screen.queryByText(/ended without a known result/)).toBeNull();
});
