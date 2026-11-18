// SPDX-License-Identifier: Apache-2.0
//
// A task typed in the message box starts from there when nothing in Review
// needs choosing: one quiet line says what it starts as, Send starts exactly
// that, and anything that asks the person something still opens Review.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, freshReview } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import type { Command } from "../../protocol/types";
import Talk from "./Talk";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
const tick = async (ms = 300) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

const created = () => Object.values(app.session.commands.get()).map((c) => c.body).filter((b): b is Extract<Command, { command: "create_task" }> => b.command === "create_task");
const line = () => document.querySelector(".composer__launch-line")?.textContent ?? null;
const sendButton = () => document.querySelector<HTMLButtonElement>(".composer__send")!;

async function typeIn(projectId: string, text: string, then?: () => void) {
  app.ui.selectProject(projectId);
  app.ui.selectTask(projectId, null);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}><Talk route={{ screen: "talk" }} /></RouteProvider></AppProvider>);
  await tick();
  fireEvent.change(screen.getAllByRole("textbox").at(-1)!, { target: { value: text } });
  // Choices kept on a draft with words in it, as Review and the tools keep them.
  if (then) act(then);
  await tick();
}

it("starts a typed change from the message box exactly as its line says", async () => {
  await typeIn(ids.pNotes, "Tidy the field notes index");
  expect(line()).toMatch(/^Change · changes the project folder · .+ · starts now$/);
  expect(sendButton().getAttribute("aria-label")).toBe("Start task");
  fireEvent.click(sendButton());
  await tick(3000);
  expect(app.ui.get().stacks.talk).toEqual([{ screen: "talk" }]);
  const [c] = created();
  expect(c).toMatchObject({ project_id: ids.pNotes, mode: "work", prompt: "Tidy the field notes index", title: null, placement: { mode: "start_now" }, mission: null });
  // A typed task is no suggestion: nothing for the computer to bind it to, and no reach of its own.
  expect(c).not.toHaveProperty("origin");
  expect(c).not.toHaveProperty("access");
  expect(c).not.toHaveProperty("github");
  expect(app.ui.draft(ids.pNotes, null)?.text ?? "").toBe("");
});

it("says a change gets its own pull request and waits its turn where the project has both", async () => {
  await typeIn(ids.pTide, "Retry the flaky upload once");
  expect(line()).toMatch(/^Change · own branch and pull request · .+ · starts after “.+”$/);
  expect(sendButton().getAttribute("aria-label")).toBe("Add to Next up");
  fireEvent.click(sendButton());
  await tick(3000);
  expect(created()[0]).toMatchObject({ github: { type: "new_pr" }, placement: { mode: "queue", authorization: "auto_start" } });
});

it("leaves the pull request off once the person took it off", async () => {
  await typeIn(ids.pTide, "Retry the flaky upload once", () => app.ui.saveDraft(ids.pTide, null, { review: { ...freshReview(), github: null } }));
  expect(line()).toMatch(/^Change · changes the project folder · /);
  fireEvent.click(sendButton());
  await tick(3000);
  expect(created()[0]).not.toHaveProperty("github");
});

it("opens Review from the line, to see or change the settings first", async () => {
  await typeIn(ids.pNotes, "Tidy the field notes index");
  fireEvent.click(document.querySelector(".composer__launch")!);
  expect(app.ui.get().stacks.talk.at(-1)).toMatchObject({ screen: "review", projectId: ids.pNotes, taskId: null, intent: "new_task" });
  expect(created()).toHaveLength(0);
});

it.each([
  ["a Plan", () => app.ui.saveDraft(ids.pNotes, null, { mode: "plan" })],
  ["a stopping point chosen in Review", () => app.ui.saveDraft(ids.pNotes, null, { review: { ...freshReview(), stopWhen: "When the index builds" } })],
  ["a branch chosen in Review", () => app.ui.saveDraft(ids.pNotes, null, { review: { ...freshReview(), branch: { name: "notes/index" } } })],
])("still goes through Review for %s", async (_what, setUp) => {
  await typeIn(ids.pNotes, "Tidy the field notes index", setUp);
  expect(line()).toBeNull();
  expect(sendButton().getAttribute("aria-label")).toBe("Review");
  fireEvent.click(sendButton());
  await tick();
  expect(app.ui.get().stacks.talk.at(-1)).toMatchObject({ screen: "review" });
  expect(created()).toHaveLength(0);
});

it("keeps the merge-by-itself confirmation in Review", async () => {
  const gh = window.__sim!.host.github;
  gh.settings.set(ids.pTide, { ...gh.settings.get(ids.pTide)!, mode: "through_merge", warning_answered: false });
  await typeIn(ids.pTide, "Retry the flaky upload once");
  expect(line()).toBeNull();
  expect(sendButton().getAttribute("aria-label")).toBe("Review");
});

it("goes through Review while Next up is paused, so a queued start never waits unseen", async () => {
  const sim = window.__sim!.host;
  sim.queue = { ...sim.queue, paused: true, revision: sim.queue.revision + 1 };
  sim.emit({ type: "queue_changed", queue: sim.queue });
  await tick(300);
  await typeIn(ids.pTide, "Retry the flaky upload once");
  if (!app.session.state.get().queues[ids.pTide]?.paused) throw new Error("the queue did not pause");
  expect(line()).toBeNull();
  expect(sendButton().getAttribute("aria-label")).toBe("Review");
});

it("starts one task for two fast taps", async () => {
  await typeIn(ids.pNotes, "Tidy the field notes index");
  const button = sendButton();
  // Both taps reach the same rendered handler before React shows it busy.
  act(() => { button.click(); button.click(); });
  await tick(3000);
  expect(created()).toHaveLength(1);
});
