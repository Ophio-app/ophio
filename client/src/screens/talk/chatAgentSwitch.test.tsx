// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { RouteProvider } from "../../shell/Screen";
import { stopStore } from "../control/stops";
import Talk from "./Talk";
import { appLock, unavailableLock } from "../../platform/lock";
import { CHAT_AGENT_SWITCH_FEATURE } from "./conversation";
import { readEffortMemory } from "./effortMemory";
import { claudeModels } from "../../dev/sim/modelCatalog";

let app: App;
const tick = (ms = 100) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  appLock(platform).accept(unavailableLock);
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  // This fixture simulates an up-to-date second agent that supports Ask.
  session.state.set(s => ({ ...s, harnesses: s.harnesses.map(h => h.kind === "codex" ? { ...h, modes: s.harnesses.find(x => x.kind === "claude_code")!.modes } : h) }));
  app.ui.selectProject(ids.pTide);
  const route = { screen: "talk" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /></RouteProvider></AppProvider>);
  await tick();
});
afterEach(() => { cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
const task = () => window.__sim!.host.tasks.find(t => t.conversation)!;
const composer = () => screen.getAllByRole("textbox").at(-1)!;
async function begin() {
  fireEvent.click(screen.getByRole("button", { name: "New conversation" }));
  await tick();
  fireEvent.change(composer(), { target: { value: "Compare two ways to retry a request." } });
  fireEvent.click(screen.getByRole("button", { name: "Start conversation" }));
  await tick(4000);
}
async function choose(name: string, running = false) {
  fireEvent.click(screen.getByRole("button", { name: /^Agent for your next message:/ }));
  await tick();
  const sheet = within(screen.getByRole("dialog", { name: "Agent for your next message" }));
  expect(sheet.getByText(running ? "After this reply, in this chat only." : "In this chat only.")).toBeTruthy();
  fireEvent.click(within(sheet.getByRole("group", { name: "Agent" })).getByRole("button", { name }));
  fireEvent.click(sheet.getByRole("button", { name: "Use from the next message" }));
  await tick();
}
async function say(words: string, ms = 4000) {
  fireEvent.change(composer(), { target: { value: words } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await tick(ms);
}

it("switches and returns in the same chat with durable handover lines and truthful old replies", async () => {
  await begin();
  const id = task().task_id;
  const memory = readEffortMemory(app.profile.profileId);
  const project = { ...app.session.state.get().projects[ids.pTide]! };
  await choose("Codex");
  expect(task().harness).toBe("claude_code");
  expect(screen.getByRole("button", { name: /^Agent for your next message: Codex/ }).textContent).toContain("Next · Codex");
  const command = vi.spyOn(app.session, "command");
  await say("Check that comparison with Codex.");
  expect(command).toHaveBeenCalledWith(expect.objectContaining({ command: "resume_task", task_id: id, harness: "codex" }), expect.anything());
  expect(task()).toMatchObject({ task_id: id, harness: "codex", conversation: true });
  expect(screen.getByText("Codex takes over with the recent conversation, not the other agent’s own memory.")).toBeTruthy();
  expect(document.querySelectorAll(".talk-agent__speaker")[0]?.textContent).toMatch(/^Claude Code/);
  expect([...document.querySelectorAll(".talk-agent__speaker")].at(-1)?.textContent).toMatch(/^Codex/);
  await choose("Claude Code");
  await say("Now carry on with the first agent.");
  expect(task()).toMatchObject({ task_id: id, harness: "claude_code" });
  expect(screen.getByRole("button", { name: /^Agent for your next message:/ }).textContent).not.toContain("Next ·");
  expect(screen.getByText("Claude Code returns with the recent conversation, not the other agent’s own memory.")).toBeTruthy();
  expect(readEffortMemory(app.profile.profileId)).toEqual(memory);
  expect(app.session.state.get().projects[ids.pTide]?.default_harness).toBe(project.default_harness);
  expect(command.mock.calls.some(([c]) => c.command === "update_project_defaults")).toBe(false);
});

it("restores each provider's model and effort after switching, remounting the app and reconnecting", async () => {
  window.__sim!.host.harnesses = app.session.state.get().harnesses.map(h => h.kind === "claude_code" ? { ...h, models: structuredClone(claudeModels) } : h);
  act(() => app.session.state.set(s => ({ ...s, harnesses: window.__sim!.host.harnesses })));
  await begin();
  // Apply an explicit small model in this conversation, without changing the
  // profile's task defaults. All replies here are the local simulated host.
  fireEvent.click(screen.getByRole("button", { name: /^Agent for your next message:/ }));
  await tick();
  let sheet = within(screen.getByRole("dialog", { name: "Agent for your next message" }));
  fireEvent.click(sheet.getByRole("button", { name: "All models" }));
  fireEvent.click(sheet.getByRole("button", { name: "Haiku 4.5" }));
  fireEvent.click(within(sheet.getByRole("group", { name: "Effort" })).getByRole("button", { name: "Low" }));
  fireEvent.click(sheet.getByRole("button", { name: "Use from the next message" }));
  await tick();
  await say("Keep this model for this provider.");
  expect(task()).toMatchObject({ harness: "claude_code", model: "haiku4.5", effort: "low" });
  await choose("Codex");
  await say("Let Codex check it.");
  const id = task().task_id;
  const codex = { model: task().model, effort: task().effort };
  cleanup();
  stopStore(app.session).dispose();
  app.session.stop();
  // A fresh client session must reconstruct from the host's saved runs and
  // the conversation cache, not from refs left alive in the old picker.
  app = { ...app, ui: new UiStore(app.profile.profileId), session: new HostSession(app.profile.profileId, app.platform.control(app.profile.profileId)) };
  app.session.start();
  await tick(900);
  act(() => app.ui.openTask(ids.pTide, id));
  const route = { screen: "talk" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /></RouteProvider></AppProvider>);
  await tick();
  fireEvent.click(screen.getByRole("button", { name: /^Agent for your next message:/ }));
  await tick();
  sheet = within(screen.getByRole("dialog", { name: "Agent for your next message" }));
  const agents = within(sheet.getByRole("group", { name: "Agent" }));
  fireEvent.click(agents.getByRole("button", { name: "Claude Code" }));
  expect(sheet.getByRole("button", { name: "Haiku 4.5" }).getAttribute("aria-pressed")).toBe("true");
  expect(within(sheet.getByRole("group", { name: "Effort" })).getByRole("button", { name: "Low" }).getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(agents.getByRole("button", { name: "Codex" }));
  fireEvent.click(sheet.getByRole("button", { name: "Use from the next message" }));
  await tick();
  expect(app.ui.draft(ids.pTide, id)?.agent).toMatchObject({ harness: "codex", ...codex });
  await choose("Claude Code");
  const command = vi.spyOn(app.session, "command");
  await say("Use the earlier small model again.");
  expect(command).toHaveBeenCalledWith(expect.objectContaining({ command: "resume_task", task_id: id, harness: "claude_code", model: "haiku4.5", effort: "low" }), expect.anything());
});

it("shows a durable handover even when earlier runs are not loaded, and never invents one", async () => {
  await begin();
  await choose("Codex");
  await say("Check the comparison.");
  const id = task().task_id;
  const runId = task().current_run!;
  act(() => app.session.state.set(s => ({
    ...s,
    runs: { [runId]: s.runs[runId]! },
    timelines: { ...s.timelines, [id]: { ...s.timelines[id]!, items: s.timelines[id]!.items.filter(i => i.run_id === runId), hasMore: true } },
  })));
  expect(screen.getByText("Codex takes over with the recent conversation, not the other agent’s own memory.")).toBeTruthy();
  act(() => app.session.state.set(s => ({ ...s, runs: { [runId]: { ...s.runs[runId]!, chat_handover: undefined } } })));
  expect(screen.queryByText(/takes over with the recent conversation/)).toBeNull();
});

it("keeps the running reply's name and queues a different agent instead of steering it", async () => {
  await begin();
  await say("Take another look.", 400);
  const current = task().current_run;
  await choose("Codex", true);
  expect(screen.getByText(/^Replying · Claude Code/)).toBeTruthy();
  const command = vi.spyOn(app.session, "command");
  fireEvent.change(composer(), { target: { value: "Have Codex compare it next." } });
  expect(screen.getByRole("button", { name: /How to send: After this turn/ })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await tick(300);
  expect(command).toHaveBeenCalledWith(expect.objectContaining({ command: "send_message", delivery: "next_turn", harness: "codex" }), expect.anything());
  expect(task().current_run).toBe(current);
  await tick(12000);
  expect(task().harness).toBe("codex");
});

it("sends to a ready destination even when the old agent cannot start and the destination cannot resume", async () => {
  await begin();
  await choose("Codex");
  act(() => app.session.state.set(s => ({ ...s, harnesses: s.harnesses.map(h => ({ ...h, capabilities: { ...h.capabilities,
    ...(h.kind === "claude_code" ? { start_run: { state: "unsupported" as const, reason: "Old agent unavailable" } } : {}),
    resume_session: { state: "unsupported" as const, reason: "No saved sessions" },
  } })) })));
  fireEvent.change(composer(), { target: { value: "Ask the other agent." } });
  expect(screen.getByRole("textbox", { name: "Reply to Codex" })).toBe(composer());
  expect(screen.getByRole("button", { name: "Send" }).hasAttribute("disabled")).toBe(false);
  expect(screen.queryByText(/Old agent unavailable|fresh session|won’t remember/)).toBeNull();
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await tick();
  expect(command).toHaveBeenCalledWith(expect.objectContaining({ command: "resume_task", task_id: task().task_id, harness: "codex" }), expect.anything());
});

it("disables Send if the selected destination becomes unavailable", async () => {
  await begin();
  await choose("Codex");
  act(() => app.session.state.set(s => ({ ...s, harnesses: s.harnesses.map(h => h.kind === "codex" ? { ...h, capabilities: { ...h.capabilities, start_run: { state: "unsupported", reason: "Codex is unavailable" } } } : h) })));
  fireEvent.change(composer(), { target: { value: "Wait for Codex." } });
  expect(screen.getByRole("button", { name: "Send" }).hasAttribute("disabled")).toBe(true);
  expect(screen.getByText("Codex is unavailable")).toBeTruthy();
});

it("leaves one-agent chats unlabeled across replies, model changes and a pending switch", async () => {
  await begin();
  await say("Keep explaining.");
  const runId = task().current_run!;
  act(() => app.session.state.set(s => ({ ...s, runs: { ...s.runs, [runId]: { ...s.runs[runId]!, actual_model: "claude-sonnet-5" } } })));
  expect(document.querySelectorAll(".talk-agent__speaker")).toHaveLength(0);
  await choose("Codex");
  expect(document.querySelectorAll(".talk-agent__speaker")).toHaveLength(0);
});

it("labels earlier and live replies only when a second agent actually replies in this chat", async () => {
  await begin();
  const chat = task();
  const oldRun = app.session.state.get().runs[chat.current_run!]!;
  const runId = "live-other-agent";
  act(() => app.session.state.set(s => ({
    ...s,
    runs: { ...s.runs, [runId]: { ...oldRun, run_id: runId, harness: "codex", actual_model: null, requested_model: null } },
    liveText: {
      unrelated: { key: "unrelated", taskId: "another-chat", runId, text: "Elsewhere", at: Date.now() },
      pending: { key: "pending", taskId: chat.task_id, runId, text: "", at: Date.now() },
      same: { key: "same", taskId: chat.task_id, runId: oldRun.run_id, text: "More from the first agent", at: Date.now() },
    },
  })));
  expect(document.querySelectorAll(".talk-agent__speaker")).toHaveLength(0);
  act(() => app.session.state.set(s => ({ ...s, liveText: { ...s.liveText, pending: { ...s.liveText.pending!, text: "Another agent is replying" } } })));
  expect(document.querySelector(".talk-agent:not(.talk-agent--live) .talk-agent__speaker")?.textContent).toMatch(/^Claude Code/);
  const liveReply = screen.getByText("Another agent is replying").closest(".talk-agent--live");
  expect(liveReply?.querySelector(".talk-agent__speaker")?.textContent).toMatch(/^Codex/);
  expect(screen.queryByText("Elsewhere")).toBeNull();
});

it("keeps old companions model-only and sends no agent override", async () => {
  await begin();
  act(() => app.session.state.set(s => ({ ...s, capabilities: s.capabilities ? { ...s.capabilities, features: s.capabilities.features.filter(f => f !== CHAT_AGENT_SWITCH_FEATURE) } : null })));
  fireEvent.click(screen.getByRole("button", { name: /^Model for your next message:/ }));
  await tick();
  const sheet = within(screen.getByRole("dialog", { name: "Model for your next message" }));
  expect(sheet.queryByRole("group", { name: "Agent" })).toBeNull();
  fireEvent.click(sheet.getByRole("button", { name: "Use from the next message" }));
  await tick();
  const command = vi.spyOn(app.session, "command");
  await say("Keep the original agent.");
  const resumed = command.mock.calls.find(([c]) => c.command === "resume_task")?.[0];
  expect(resumed).toBeTruthy();
  expect(resumed).not.toHaveProperty("harness");
});

it("names the replying agent apart only when the next message changes it", async () => {
  await begin();
  await say("Take another look.", 400);
  // The same agent for the next message: the chip alone says who is replying.
  expect(screen.queryByText(/^Replying · /)).toBeNull();
  expect(screen.getByRole("button", { name: /^Agent for your next message: Claude Code/ })).toBeTruthy();
  await choose("Codex", true);
  expect(screen.getByText(/^Replying · Claude Code/)).toBeTruthy();
});
