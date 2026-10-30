// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { tasks } from "../../dev/sim/scenario";
import { taskMatches } from "./logic";
it("finds a task by its title, request or status, whatever the case", () => {
  const first = tasks(Date.now())[0];
  if (!first) throw new Error("Missing task fixture");
  const archived = { ...first, title: "Removed request", state: "archived" as const };
  expect(taskMatches(archived, "removed")).toBe(true);
  expect(taskMatches(archived, " REQUEST ")).toBe(true);
  expect(taskMatches(archived, "")).toBe(true);
  expect(taskMatches(archived, "nothing like it")).toBe(false);
});

import { emptyHostState, openAttention, reviewAttention } from "../../state/projection";
import type { AttentionItem } from "../../protocol/types";
it("uses current task titles and hides archived attention from both groups", () => {
  const task = { ...tasks(Date.now())[0]!, title: "Current title" };
  const item: AttentionItem = { attention_id: "attention", kind: "waiting_for_you", task_id: task.task_id, run_id: task.current_run, title: "Old title", body: null, approval_id: null, priority: 1, created_at: 1, resolved_at: null };
  const state = { ...emptyHostState, tasks: { [task.task_id]: task }, attention: { attention: item } };
  expect(openAttention(state)[0]?.title).toBe("Current title");
  const archived = { ...state, tasks: { [task.task_id]: { ...task, state: "archived" as const } } };
  expect(openAttention(archived)).toEqual([]);
  expect(reviewAttention({ ...archived, attention: { attention: { ...item, kind: "completed" } } })).toEqual([]);
});
