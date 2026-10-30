// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { tasks, runs } from "../../dev/sim/scenario";
import type { HostState } from "../../state/projection";
import type { AttentionItem, Run, Task } from "../../protocol/types";
import { resultLabel } from "./attention";

const now = 100_000_000;
const base = tasks(now)[1]!;
const first = { ...runs(now)[1]!, task_id: base.task_id, run_id: "run-1", admitted_at: now - 2000, state: "completed", mode: "plan" } as Run;
const item = { attention_id: "a", kind: "completed", task_id: base.task_id, run_id: "run-1", created_at: now } as AttentionItem;
const state = (task: Task, ...list: Run[]) => ({ tasks: { [task.task_id]: task }, runs: Object.fromEntries(list.map(r => [r.run_id, r])) }) as unknown as HostState;

it("names a Plan-mode run's result as the plan it proposed", () => {
  const task = { ...base, mode: "plan", state: "completed", current_run: "run-1" } as Task;
  expect(resultLabel(state(task, first), item)).toBe("Plan proposed");
  expect(resultLabel(state(task, { ...first, mode: "work" }), item)).toBe("Finished");
});

it("names both runs when a proposed plan was carried out and stopped", () => {
  const second = { ...first, run_id: "run-2", admitted_at: now - 1000, state: "interrupted", mode: "work" } as Run;
  const task = { ...base, mode: "work", state: "interrupted", current_run: "run-2" } as Task;
  expect(resultLabel(state(task, first, second), item)).toBe("Run 1 proposed a plan · run 2 stopped");
});
