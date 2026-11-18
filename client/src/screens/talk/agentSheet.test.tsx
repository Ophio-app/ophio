// SPDX-License-Identifier: Apache-2.0
import { useState, type ComponentProps } from "react";
import { act, cleanup, fireEvent, isInaccessible, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type AgentChoice } from "../../app/ui";
import { claudeModels } from "../../dev/sim/modelCatalog";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { AgentSheet } from "./AgentSheet";
import { readEffortMemory, saveEffortMemory } from "./effortMemory";
import { pickerModelLabel } from "./modelPicker";
import type { Task } from "../../protocol/types";

let app: App;
beforeEach(async () => {
  localStorage.clear();
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start();
  await vi.advanceTimersByTimeAsync(900);
  session.state.set(s => ({ ...s, harnesses: s.harnesses.map(h => h.kind === "claude_code" ? { ...h, models: structuredClone(claudeModels) } : h) }));
});
afterEach(() => {
  cleanup();
  app.session.stop();
  window.__sim?.host.dispose();
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.restoreAllMocks();
});
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const fixedExplanation = "Fixed versions stay on that version when a newer one comes out.";
const visibleFixedExplanations = () => screen.queryAllByText(fixedExplanation).filter(element => !isInaccessible(element));
const models = () => within(screen.getByRole("group", { name: "Model" }));
const efforts = () => within(screen.getByRole("group", { name: "Effort" }));
const modelNames = () => models().getAllByRole("button").map(button => button.textContent);
const selectedModel = (name: string) => expect(models().getByRole("button", { name }).getAttribute("aria-pressed")).toBe("true");

function mountSheet(options: Partial<ComponentProps<typeof AgentSheet>> = {}) {
  const onChange = vi.fn();
  const onClose = vi.fn();
  const project = app.session.state.get().projects[ids.pTide]!;
  function Parent() {
    const [open, setOpen] = useState(true);
    const [value, setValue] = useState<AgentChoice>(options.value ?? { harness: "claude_code", model: null, effort: "high", launchPolicy: "standard" });
    return <AppProvider app={app}>
      <button onClick={() => setOpen(true)}>Open agent picker</button>
      <AgentSheet project={project} {...options} open={open} value={value}
        onChange={choice => { onChange(choice); setValue(choice); }}
        onClose={() => { onClose(); setOpen(false); }} />
    </AppProvider>;
  }
  const mounted = render(<Parent />);
  return { ...mounted, onChange, onClose, project };
}

function chooseModel(name: string) {
  const visible = models().queryByRole("button", { name });
  if (visible) { fireEvent.click(visible); return; }
  if (!screen.queryByRole("group", { name: "More models" })) fireEvent.click(screen.getByRole("button", { name: "All models" }));
  fireEvent.click(within(screen.getByRole("group", { name: "More models" })).getByRole("button", { name }));
}

it("starts with the agent default and four current choices, without fixed-version explanation", () => {
  mountSheet();
  expect(claudeModels).toHaveLength(17);
  expect(modelNames()).toEqual(["Agent’s default", "Opus 5.5", "Fable 5.2", "Sonnet 5.5", "Haiku 5.5"]);
  selectedModel("Agent’s default");
  expect(screen.getByRole("button", { name: "All models" })).toBeTruthy();
  expect(screen.queryByRole("group", { name: "More models" })).toBeNull();
  expect(visibleFixedExplanations()).toHaveLength(0);
  expect(efforts().getByRole("button", { name: "High" }).getAttribute("aria-pressed")).toBe("true");
});

it("discloses all 18 choices and gives exact versions readable, distinct labels", () => {
  mountSheet();
  fireEvent.click(screen.getByRole("button", { name: "All models" }));
  const more = within(screen.getByRole("group", { name: "More models" }));
  const names = [...modelNames(), ...more.getAllByRole("button").map(button => button.textContent)];
  expect(names).toHaveLength(18);
  expect(new Set(names).size).toBe(18);
  const harness = app.session.state.get().harnesses.find(h => h.kind === "claude_code")!;
  expect(names).toEqual(expect.arrayContaining(["Agent’s default", ...claudeModels.map(model => pickerModelLabel(model, harness.display_name))]));
  expect(more.getAllByRole("button").map(button => button.textContent)).toEqual([
    "Fable 5.1", "Fable 5", "Haiku 4.5", "Opus 5", "Opus 4.8", "Opus 4.7", "Opus 4.6", "Sonnet 4.6",
    "Fable 5.2 · fixed", "Haiku 5.5 · fixed", "Opus 5.5 · fixed", "Sonnet 5.5 · fixed", "Sonnet 5 · fixed",
  ]);
  expect(more.getByRole("button", { name: "Fable 5.2 · fixed" }).parentElement?.previousElementSibling?.textContent).toBe(fixedExplanation);
  expect(more.getByRole("button", { name: "Opus 5.5 · fixed" })).toBeTruthy();
  expect(more.getByRole("button", { name: "Sonnet 5 · fixed" })).toBeTruthy();
  expect(visibleFixedExplanations()).toHaveLength(1);
  fireEvent.click(screen.getByRole("button", { name: "Fewer models" }));
  expect(screen.queryByRole("group", { name: "More models" })).toBeNull();
  expect(visibleFixedExplanations()).toHaveLength(0);
});

it.each([
  ["opus4.6", "Opus 4.6", false],
  ["claude-opus-5-5", "Opus 5.5 · fixed", true],
] as const)("keeps an older or exact choice visible after collapsing and reopening (%s)", async (id, name, fixed) => {
  const { onChange } = mountSheet();
  chooseModel(name);
  selectedModel(name);
  expect(document.activeElement).toBe(models().getByRole("button", { name }));
  expect(models().getAllByRole("button").length).toBeLessThanOrEqual(5);
  expect(within(screen.getByRole("group", { name: "More models" })).queryByRole("button", { name })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Fewer models" }));
  selectedModel(name);
  expect(visibleFixedExplanations()).toHaveLength(fixed ? 1 : 0);
  expect(onChange).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Use this agent" }));
  await tick();
  expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({ model: id }));
  fireEvent.click(screen.getByRole("button", { name: "Open agent picker" }));
  selectedModel(name);
  expect(screen.queryByRole("group", { name: "More models" })).toBeNull();
  expect(visibleFixedExplanations()).toHaveLength(fixed ? 1 : 0);
});

it("does not reshuffle the opening shortlist as choices and effort memory change", () => {
  mountSheet();
  const initial = modelNames();
  for (const name of ["Sonnet 5.5", "Fable 5.2", "Haiku 5.5", "Opus 5.5"]) {
    chooseModel(name);
    fireEvent.click(efforts().getByRole("button", { name: "Low" }));
    selectedModel(name);
    expect(modelNames()).toEqual(initial);
  }
});

it("keeps opening remembered choices stable even after the live effort memory changes", () => {
  saveEffortMemory(app.profile.profileId, {
    models: { claude_code: "opus4.6" },
    efforts: { "claude_code/claude-opus-5-5": "low" },
  });
  mountSheet();
  const initial = modelNames();
  expect(initial).toContain("Opus 4.6");
  expect(initial).not.toContain("Opus 5.5 · fixed");
  expect(visibleFixedExplanations()).toHaveLength(0);
  chooseModel("Fable 5.2");
  fireEvent.click(efforts().getByRole("button", { name: "Medium" }));
  chooseModel("Opus 5.5");
  expect(modelNames()).toEqual(initial);
  selectedModel("Opus 5.5");
});

it("restores effort separately for a shortlisted model and an exact model", async () => {
  mountSheet({ value: { harness: "claude_code", model: "opus", effort: "high", launchPolicy: "standard" } });
  fireEvent.click(efforts().getByRole("button", { name: "Low" }));
  chooseModel("Opus 5.5 · fixed");
  fireEvent.click(efforts().getByRole("button", { name: "Maximum" }));
  chooseModel("Opus 5.5");
  expect(efforts().getByRole("button", { name: "Low" }).getAttribute("aria-pressed")).toBe("true");
  chooseModel("Opus 5.5 · fixed");
  expect(efforts().getByRole("button", { name: "Maximum" }).getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(screen.getByRole("button", { name: "Use this agent" }));
  await tick();
  const memory = readEffortMemory(app.profile.profileId);
  expect(memory.efforts["claude_code/opus"]).toBe("low");
  expect(memory.efforts["claude_code/claude-opus-5-5"]).toBe("max");
  fireEvent.click(screen.getByRole("button", { name: "Open agent picker" }));
  chooseModel("Opus 5.5");
  expect(efforts().getByRole("button", { name: "Low" }).getAttribute("aria-pressed")).toBe("true");
});

it("applies compact picks immediately and keeps them after closing without saving profile memory", async () => {
  const { onChange, onClose } = mountSheet({ compact: true });
  chooseModel("Opus 5.5 · fixed");
  expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({ model: "claude-opus-5-5", effort: "high" }));
  fireEvent.click(efforts().getByRole("button", { name: "Maximum" }));
  expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({ model: "claude-opus-5-5", effort: "max" }));
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  await tick();
  expect(onClose).toHaveBeenCalledOnce();
  expect(readEffortMemory(app.profile.profileId)).toEqual({ efforts: {}, models: {} });
  fireEvent.click(screen.getByRole("button", { name: "Open agent picker" }));
  selectedModel("Opus 5.5 · fixed");
  expect(efforts().getByRole("button", { name: "Maximum" }).getAttribute("aria-pressed")).toBe("true");
  expect(screen.queryByRole("group", { name: "More models" })).toBeNull();
});

it("saves compact per-model effort memory when Done is used", async () => {
  const { onClose } = mountSheet({ compact: true });
  chooseModel("Opus 5.5 · fixed");
  fireEvent.click(efforts().getByRole("button", { name: "Maximum" }));
  fireEvent.click(screen.getByRole("button", { name: "Use this agent" }));
  await tick();
  expect(onClose).toHaveBeenCalledOnce();
  const memory = readEffortMemory(app.profile.profileId);
  expect(memory.models.claude_code).toBe("claude-opus-5-5");
  expect(memory.efforts["claude_code/claude-opus-5-5"]).toBe("max");
});

it("still saves full-picker project defaults and the chosen permission prompts without starting work", async () => {
  const command = vi.spyOn(app.session, "command");
  const { onChange, onClose, project } = mountSheet();
  chooseModel("Sonnet 5.5");
  fireEvent.click(efforts().getByRole("button", { name: "Medium" }));
  const permissions = within(screen.getByRole("group", { name: "Permission prompts" }));
  fireEvent.click(permissions.getByRole("button", { name: "Accept edits" }));
  expect(permissions.getByRole("button", { name: "Accept edits" }).getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(screen.getByRole("button", { name: `Also use this agent, model and effort for new tasks in ${project.name}` }));
  expect(onChange).not.toHaveBeenCalled();
  expect(command).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Use this agent" }));
  await tick();
  expect(command).toHaveBeenCalledWith({ command: "update_project_defaults", project_id: project.project_id, harness: "claude_code", model: "sonnet", effort: "medium" }, expect.anything());
  expect(onChange).toHaveBeenLastCalledWith({ harness: "claude_code", model: "sonnet", effort: "medium", launchPolicy: "accept_edits" });
  expect(onClose).toHaveBeenCalledOnce();
  expect(app.session.state.get().projects[project.project_id]).toMatchObject({ default_harness: "claude_code", default_model: "sonnet", default_effort: "medium" });
  expect(command.mock.calls.every(([body]) => body.command === "update_project_defaults")).toBe(true);
});

it("a next-message pick never changes saved model, effort or project defaults", async () => {
  app.session.state.set(s => ({ ...s, harnesses: s.harnesses.map(h => h.kind === "codex" ? { ...h, modes: s.harnesses.find(x => x.kind === "claude_code")!.modes } : h) }));
  const before = readEffortMemory(app.profile.profileId);
  const command = vi.spyOn(app.session, "command");
  const { onChange } = mountSheet({ nextMessage: true, launchFlags: false, mode: "ask" });
  fireEvent.click(within(screen.getByRole("group", { name: "Agent" })).getByRole("button", { name: "Codex" }));
  expect(screen.queryByRole("button", { name: /Also use this agent/ })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Use this agent" }));
  await tick();
  expect(onChange).toHaveBeenCalledWith(expect.objectContaining({ harness: "codex" }));
  expect(readEffortMemory(app.profile.profileId)).toEqual(before);
  expect(command).not.toHaveBeenCalled();
});

it("lists an unavailable conversation agent with its reason and disables it", () => {
  app.session.state.set(s => ({ ...s, harnesses: s.harnesses.map(h => h.kind === "codex" ? { ...h, account: { state: "signed_out" as const, remedy: "Sign in again on the computer." } } : h) }));
  mountSheet({ nextMessage: true, launchFlags: false, mode: "ask" });
  const codex = within(screen.getByRole("group", { name: "Agent" })).getByRole("button", { name: "Codex" });
  expect(codex.hasAttribute("disabled")).toBe(true);
  expect(screen.getByText(/Codex: Not signed in/)).toBeTruthy();
});

function savedConversation(model: string | null = "haiku4.5", effort: string | null = "low") {
  const state = app.session.state.get();
  const templateTask = Object.values(state.tasks)[0]!;
  const templateRun = Object.values(state.runs)[0]!;
  const task: Task = { ...templateTask, task_id: "memory-chat", conversation: true, harness: "codex", model: "gpt-6-astra", effort: "high", current_run: "latest-codex", updated_at: 30 };
  const current = { ...templateRun, task_id: task.task_id, run_id: "latest-codex", harness: "codex" as const, requested_model: task.model, requested_effort: task.effort, admitted_at: 30 };
  const earlier = { ...templateRun, task_id: task.task_id, run_id: "earlier-claude", harness: "claude_code" as const, requested_model: model, requested_effort: effort, actual_model: "claude-opus-5-5", actual_effort: "high", admitted_at: 20 };
  app.session.state.set(s => ({ ...s,
    harnesses: s.harnesses.map(h => h.kind === "codex" ? { ...h, modes: s.harnesses.find(x => x.kind === "claude_code")!.modes } : h),
    runs: { ...s.runs, [current.run_id]: current, [earlier.run_id]: earlier },
  }));
  return task;
}
const chooseAgent = (name: string) => fireEvent.click(within(screen.getByRole("group", { name: "Agent" })).getByRole("button", { name }));

it("restores a chat's Haiku and effort after a cold picker and again when reconnect history omits that provider", async () => {
  const chat = savedConversation();
  saveEffortMemory(app.profile.profileId, { models: { claude_code: "opus" }, efforts: { "claude_code/opus": "max" } });
  const options = { nextMessage: true, launchFlags: false, mode: "ask" as const, conversationTask: chat, value: { harness: chat.harness, model: chat.model, effort: chat.effort } };
  const first = mountSheet(options);
  chooseAgent("Claude Code");
  selectedModel("Haiku 4.5");
  expect(efforts().getByRole("button", { name: "Low" }).getAttribute("aria-pressed")).toBe("true");
  chooseAgent("Codex");
  selectedModel("GPT-6-Astra");
  expect(efforts().getByRole("button", { name: "High" }).getAttribute("aria-pressed")).toBe("true");
  first.unmount();
  act(() => app.session.state.set(s => ({ ...s, runs: { "latest-codex": s.runs["latest-codex"]! } })));
  const cold = mountSheet(options);
  chooseAgent("Claude Code");
  selectedModel("Haiku 4.5");
  fireEvent.click(screen.getByRole("button", { name: "Use this agent" }));
  await tick();
  expect(cold.onChange).toHaveBeenLastCalledWith(expect.objectContaining({ harness: "claude_code", model: "haiku4.5", effort: "low" }));
  expect(readEffortMemory(app.profile.profileId).models.claude_code).toBe("opus");
});

it("keeps an explicit current model and effort ahead of saved history while switching away and back", () => {
  const chat = savedConversation();
  mountSheet({ nextMessage: true, launchFlags: false, mode: "ask", conversationTask: chat, value: { harness: "claude_code", model: "sonnet", effort: "medium" } });
  chooseAgent("Codex");
  chooseAgent("Claude Code");
  selectedModel("Sonnet 5.5");
  expect(efforts().getByRole("button", { name: "Medium" }).getAttribute("aria-pressed")).toBe("true");
});

it("explains a removed historical model instead of claiming to restore it", () => {
  const chat = savedConversation("removed-model", "max");
  mountSheet({ nextMessage: true, launchFlags: false, mode: "ask", conversationTask: chat, value: { harness: chat.harness, model: chat.model, effort: chat.effort } });
  chooseAgent("Claude Code");
  selectedModel("Agent’s default");
  expect(efforts().getByRole("button", { name: "Agent’s default" }).getAttribute("aria-pressed")).toBe("true");
  expect(screen.getByText(/previous model \(removed-model\) is unavailable.*default model and effort/)).toBeTruthy();
  expect(screen.queryByText(/restoring your choice/)).toBeNull();
});

it("keeps an invalid explicit current selection visible and blocked instead of silently replacing it", () => {
  const chat = savedConversation();
  mountSheet({ nextMessage: true, launchFlags: false, mode: "ask", conversationTask: chat, value: { harness: "claude_code", model: "explicit-removed-model", effort: "low" } });
  expect(screen.getByText(/selected model \(explicit-removed-model\) is unavailable/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Use this agent" }).hasAttribute("disabled")).toBe(true);
  expect(models().getByRole("button", { name: "Agent’s default" }).getAttribute("aria-pressed")).toBe("false");
});

it("restores an explicit default model and effort rather than the previous native model", () => {
  const chat = savedConversation(null, null);
  mountSheet({ nextMessage: true, launchFlags: false, mode: "ask", conversationTask: chat, value: { harness: chat.harness, model: chat.model, effort: chat.effort } });
  chooseAgent("Claude Code");
  selectedModel("Agent’s default");
  expect(efforts().getByRole("button", { name: "Agent’s default" }).getAttribute("aria-pressed")).toBe("true");
});

it("explains an unsupported historical effort while retaining its model", () => {
  const chat = savedConversation("haiku4.5", "removed-effort");
  mountSheet({ nextMessage: true, launchFlags: false, mode: "ask", conversationTask: chat, value: { harness: chat.harness, model: chat.model, effort: chat.effort } });
  chooseAgent("Claude Code");
  selectedModel("Haiku 4.5");
  expect(efforts().getByRole("button", { name: "Agent’s default" }).getAttribute("aria-pressed")).toBe("true");
  expect(screen.getByText(/doesn’t offer Removed-effort effort.*default/)).toBeTruthy();
});

it("labels an unknown provider choice honestly and does not borrow another chat's or profile's model", () => {
  const chat = savedConversation();
  app.session.state.set(s => ({ ...s, runs: { "latest-codex": s.runs["latest-codex"]! } }));
  saveEffortMemory(app.profile.profileId, { models: { claude_code: "opus" }, efforts: { "claude_code/opus": "max" } });
  mountSheet({ nextMessage: true, launchFlags: false, mode: "ask", conversationTask: { ...chat, task_id: "fresh-other-chat" }, value: { harness: chat.harness, model: chat.model, effort: chat.effort } });
  chooseAgent("Claude Code");
  selectedModel("Agent’s default");
  expect(screen.getByText(/No earlier model choice is available.*in this chat.*default model/)).toBeTruthy();
  expect(screen.queryByText(/restoring your choice/)).toBeNull();
});

it("resets disclosure when switching agents and when reopening the sheet", () => {
  mountSheet();
  fireEvent.click(screen.getByRole("button", { name: "All models" }));
  expect(screen.getByRole("group", { name: "More models" })).toBeTruthy();
  const agents = within(screen.getByRole("group", { name: "Agent" }));
  fireEvent.click(agents.getByRole("button", { name: "Codex" }));
  expect(screen.queryByRole("group", { name: "More models" })).toBeNull();
  fireEvent.click(agents.getByRole("button", { name: "Claude Code" }));
  expect(screen.queryByRole("group", { name: "More models" })).toBeNull();
  expect(screen.getByRole("button", { name: "All models" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "All models" }));
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  fireEvent.click(screen.getByRole("button", { name: "Open agent picker" }));
  expect(screen.queryByRole("group", { name: "More models" })).toBeNull();
  expect(screen.getByRole("button", { name: "All models" })).toBeTruthy();
});
