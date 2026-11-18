// SPDX-License-Identifier: Apache-2.0
import { beforeEach, expect, it } from "vitest";
import { runs, tasks } from "../../dev/sim/scenario";
import type { Run, Task } from "../../protocol/types";
import { conversationMemory, readEffortMemory, saveEffortMemory } from "./effortMemory";

beforeEach(() => localStorage.clear());
const task = (extra: Partial<Task> = {}): Task => ({ ...tasks(0)[0]!, task_id: "chat", conversation: true, harness: "codex", model: "gpt-6-astra", effort: "low", current_run: "current", updated_at: 30, ...extra });
const run = (extra: Partial<Run> = {}): Run => ({ ...runs(0)[0]!, task_id: "chat", run_id: "current", harness: "codex", requested_model: "gpt-6-astra", requested_effort: "low", admitted_at: 30, ...extra });
const haiku = run({ run_id: "haiku", harness: "claude_code", requested_model: "haiku4.5", requested_effort: "medium", actual_model: "claude-opus-5-5", actual_effort: "high", admitted_at: 20 });

it("reconstructs each provider's newest requested model and effort, not a native substitution or another chat", () => {
  const memory = conversationMemory(task(), {
    haiku,
    current: run(),
    older: { ...haiku, run_id: "older", admitted_at: 10, requested_model: "opus", requested_effort: "max" },
    other: { ...haiku, run_id: "other", admitted_at: 40, task_id: "another-chat", requested_model: "sonnet" },
  });
  expect(memory.models).toEqual({ claude_code: "haiku4.5", codex: "gpt-6-astra" });
  expect(memory.efforts["claude_code/haiku4.5"]).toBe("medium");
  expect(memory.efforts["codex/gpt-6-astra"]).toBe("low");
});

it("retains history across a cold read and a snapshot that omits older providers without changing profile preferences", () => {
  const global = { models: { claude_code: "opus" }, efforts: { "claude_code/opus": "max" } };
  saveEffortMemory("host", global);
  const memory = conversationMemory(task(), { haiku, current: run() });
  saveEffortMemory("host", memory, "chat");
  const cold = conversationMemory(task(), { current: run() }, readEffortMemory("host", "chat"));
  expect(cold.models.claude_code).toBe("haiku4.5");
  expect(cold.efforts["claude_code/haiku4.5"]).toBe("medium");
  expect(readEffortMemory("host")).toEqual(global);
  expect(readEffortMemory("other-host", "chat").models).toEqual({});
  expect(readEffortMemory("host", "another-chat").models).toEqual({});
});

it("does not replace newer cached history with an older page, but does accept newer saved choices", () => {
  const cached = conversationMemory(task(), { haiku });
  const older = conversationMemory(task(), { old: { ...haiku, admitted_at: 10, requested_model: "opus" } }, cached);
  expect(older.models.claude_code).toBe("haiku4.5");
  const newer = conversationMemory(task(), { next: { ...haiku, admitted_at: 40, run_id: "next", requested_model: "sonnet", requested_effort: "high" } }, older);
  expect(newer.models.claude_code).toBe("sonnet");
  expect(newer.efforts["claude_code/sonnet"]).toBe("high");
});

it("keeps the saved current task choice, including explicit agent defaults, ahead of run values and stale cache", () => {
  const cached = conversationMemory(task({ model: "old-model", updated_at: 100 }), {});
  const memory = conversationMemory(task({ model: null, effort: null }), { current: run() }, cached);
  expect(memory.models.codex).toBeNull();
  expect(memory.efforts["codex/default"]).toBeNull();
});
