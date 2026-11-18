// SPDX-License-Identifier: Apache-2.0
//
// Turn into a task, on a computer whose chats hear back from their work: a
// new task starts from the conversation, through the same plan a typed task
// gets, and names the chat it came from. The chat stays where it was, in
// Chat, and hears how the task ended.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { RouteProvider } from "../../shell/Screen";
import { stopStore } from "../control/stops";
import { HEARS_BACK_FEATURE } from "./workCards";
import Talk from "./Talk";
import { IntoTaskSheet } from "./IntoTask";
import { modelEfforts } from "./catalog";

let app: App;
const tick = (ms = 1500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  window.history.replaceState(null, "", "/?cards=1");
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  app.ui.selectProject(ids.pTide);
  app.ui.selectTask(ids.pTide, ids.tOneCard);
  freeWriter();
  const route = { screen: "talk" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /></RouteProvider></AppProvider>);
  await tick(600);
});
afterEach(() => {
  cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose();
  window.history.replaceState(null, "", "/");
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
});

const host = () => window.__sim!.host;

it("passes the task sheet's picked agent, model and effort to its start handler", async () => {
  cleanup();
  const start = vi.fn();
  const project = app.session.state.get().projects[ids.pTide]!;
  const route = { screen: "talk" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}>
    <IntoTaskSheet open onClose={() => {}} project={project} agent="Claude Code" access={null}
      keeps={{ line: "Change · starts now", agent: "Claude Code · Opus 5.5 · High", review: null }}
      initial="Show upload progress." blocked={null} pending={false} onStart={start} />
  </RouteProvider></AppProvider>);
  const taskSheet = () => within(screen.getByRole("dialog", { name: /Turn into a task/ }));
  fireEvent.click(taskSheet().getByRole("button", { name: /^Agent for this task:/ }));
  const picker = within(screen.getByRole("dialog", { name: /Agent for this task/ }));
  fireEvent.click(picker.getByRole("button", { name: "Codex" }));
  fireEvent.click(picker.getByRole("button", { name: "GPT-6-Astra" }));
  fireEvent.click(picker.getByRole("button", { name: "Medium" }));
  expect(picker.getByRole("button", { name: "Medium" }).getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(picker.getByRole("button", { name: "Done" }));
  expect(taskSheet().getByRole("button", { name: "Agent for this task: Codex · GPT-6-Astra · Medium" })).toBeTruthy();
  fireEvent.click(taskSheet().getByRole("button", { name: "Start the task" }));
  expect(start).toHaveBeenCalledWith("Show upload progress.", expect.objectContaining({ harness: "codex", model: "gpt-6-astra", effort: "medium" }));
  expect(host().tasks.find((t) => t.task_id === ids.tOneCard)?.harness).toBe("claude_code");
});

it("does not invent effort for the simulator's long preview model", async () => {
  const codex = host().harnesses.find((h) => h.kind === "codex")!;
  const preview = codex.models.find((m) => m.id === "gpt-6.1-sol-codex-max-preview")!;
  expect(preview).toBeTruthy();
  expect(preview.details).toBeUndefined();
  expect(modelEfforts(codex, preview.id)).toEqual([]);
  const sheet = await intoTask();
  fireEvent.click(sheet.getByRole("button", { name: /^Agent for this task:/ }));
  const picker = within(screen.getByRole("dialog", { name: /Agent for this task/ }));
  fireEvent.click(picker.getByRole("button", { name: "Codex" }));
  fireEvent.click(picker.getByRole("button", { name: preview.label }));
  expect(picker.getByText(/No effort choices reported for this model/)).toBeTruthy();
  fireEvent.click(picker.getByRole("button", { name: "Done" }));
  expect(within(screen.getByRole("dialog", { name: /Turn into a task/ })).getByRole("button", { name: `Agent for this task: Codex · ${preview.label}` })).toBeTruthy();
});

/** Frees pTide, which tFlaky holds while it waits for approval. */
function freeWriter() {
  const project = { ...host().projects.find((p) => p.project_id === ids.pTide)!, active_writer: null };
  host().projects = host().projects.map((p) => (p.project_id === ids.pTide ? project : p));
  host().emit({ type: "project_upserted", project });
}

/** Turn into a task, from the chat's menu; the sheet. */
async function intoTask() {
  fireEvent.click(screen.getByRole("button", { name: "More about this conversation" }));
  await tick(100);
  fireEvent.click(within(screen.getByRole("dialog", { name: /This conversation/ })).getByRole("button", { name: /^Turn into a task/ }));
  await tick(100);
  return within(screen.getByRole("dialog", { name: /Turn into a task/ }));
}

it("starts a new task named for the chat it came from, and the chat stays in Chat", async () => {
  expect(app.session.state.get().capabilities?.features).toContain(HEARS_BACK_FEATURE);
  const command = vi.spyOn(app.session, "command");
  const before = new Set(host().tasks.map((t) => t.task_id));
  const sheet = await intoTask();
  expect(sheet.getByText(/this chat stays/)).toBeTruthy();
  fireEvent.change(sheet.getByRole("textbox"), { target: { value: "Show upload progress beside the file name." } });
  fireEvent.click(sheet.getByRole("button", { name: "Start the task" }));
  await tick(4000);
  // One create_task, through the same plan a typed task gets, naming the chat.
  const starts = command.mock.calls.map(([c]) => c).filter((c) => c.command === "create_task");
  expect(starts).toHaveLength(1);
  expect(starts[0]).toMatchObject({ project_id: ids.pTide, from_chat: ids.tOneCard, mode: "work", prompt: expect.stringContaining("Show upload progress beside the file name.") });
  expect(starts[0]).not.toHaveProperty("origin");
  const made = host().tasks.filter((t) => !before.has(t.task_id));
  expect(made).toHaveLength(1);
  expect(made[0]).toMatchObject({ from_chat: ids.tOneCard });
  expect(made[0]!.conversation).toBeFalsy();
  // The chat is still a chat, still listed, and still the one shown.
  expect(host().tasks.find((t) => t.task_id === ids.tOneCard)).toMatchObject({ conversation: true, mode: "ask" });
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(ids.tOneCard);
  // Not the old way, which turned the chat itself into the task.
  expect(command.mock.calls.some(([c]) => c.command === "resume_task" && c.task_id === ids.tOneCard)).toBe(false);
});

it("starts the task with the agent picked in the sheet, and says what that agent will do", async () => {
  const command = vi.spyOn(app.session, "command");
  const sheet = await intoTask();
  fireEvent.click(sheet.getByRole("button", { name: /^Agent for this task:/ }));
  const picker = within(screen.getByRole("dialog", { name: /Agent for this task/ }));
  fireEvent.click(picker.getByRole("button", { name: "Codex" }));
  fireEvent.click(picker.getByRole("button", { name: "GPT-6-Astra" }));
  fireEvent.click(picker.getByRole("button", { name: "Medium" }));
  fireEvent.click(picker.getByRole("button", { name: "Done" }));
  await tick(100);
  const again = within(screen.getByRole("dialog", { name: /Turn into a task/ }));
  expect(again.getByRole("button", { name: "Agent for this task: Codex · GPT-6-Astra · Medium" })).toBeTruthy();
  fireEvent.change(again.getByRole("textbox"), { target: { value: "Show upload progress beside the file name." } });
  fireEvent.click(again.getByRole("button", { name: "Start the task" }));
  await tick(4000);
  const starts = command.mock.calls.map(([c]) => c).filter((c) => c.command === "create_task");
  expect(starts).toHaveLength(1);
  expect(starts[0]).toMatchObject({ from_chat: ids.tOneCard, harness: "codex", model: "gpt-6-astra", effort: "medium" });
});
