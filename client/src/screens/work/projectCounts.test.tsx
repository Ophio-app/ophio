// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { emptyHostState } from "../../state/projection";
import { projectCounts } from "../../shell/ProjectSheet";
import { projects, tasks, runs, ids } from "../../dev/sim/scenario";
it("separates admitted queued tasks from reported running tasks", () => {
  const now = Date.now();
  const taskList = tasks(now);
  const state = { ...emptyHostState, projects: Object.fromEntries(projects(now).map(p => [p.project_id, p])), tasks: Object.fromEntries(taskList.map(t => [t.task_id, t])), runs: Object.fromEntries(runs(now).map(r => [r.run_id, r])) };
  const counts = projectCounts(state)[ids.pTide];
  expect(counts?.queued).toBe(1);
  expect(counts?.active).toBe(2);
});

import type { AttentionItem } from "../../protocol/types";
import { elsewhere, Unread } from "../../shell/ProjectSheet";
import { render, screen } from "@testing-library/react";
it("counts each project's unread results and what waits in the others", () => {
  const now = Date.now();
  const finished = (project: string) => ({ ...tasks(now).find(t => t.project_id === project)!, state: "completed" as const, current_run: null });
  const [tide, notes] = [finished(ids.pTide), finished(ids.pNotes)];
  const item = (id: string, kind: AttentionItem["kind"], task_id: string): AttentionItem => ({ attention_id: id, kind, task_id, run_id: null, title: id, body: null, approval_id: null, priority: 1, created_at: 1, resolved_at: null });
  const state = {
    ...emptyHostState,
    projects: Object.fromEntries(projects(now).map(p => [p.project_id, p])),
    tasks: { [tide.task_id]: tide, [notes.task_id]: notes },
    attention: { a: item("a", "completed", tide.task_id), b: item("b", "failure", tide.task_id), c: item("c", "completed", notes.task_id), d: item("d", "waiting_for_you", notes.task_id), e: item("e", "failure", notes.task_id) },
  };
  expect(projectCounts(state)[ids.pTide]).toMatchObject({ unread: 2, attention: 0 });
  // What waits on a finished task is part of its result, to read.
  expect(projectCounts(state)[ids.pNotes]).toMatchObject({ unread: 3, attention: 0 });
  expect(elsewhere(state, ids.pTide)).toBe(3);
  expect(elsewhere(state, ids.pNotes)).toBe(2);
  expect(elsewhere(state, null)).toBe(5);
  render(<Unread count={2} />);
  expect(screen.getByText("unread").parentElement?.textContent).toBe("2 unread");
  expect(render(<Unread count={0} />).container.textContent).toBe("");
});

it("moves a project for a task made, a turn started or ended, or a need, not for each step", () => {
  const now = Date.now();
  const tide = { ...tasks(now).find(t => t.project_id === ids.pTide)!, created_at: 100, updated_at: 10_000 };
  const notes = { ...tasks(now).find(t => t.project_id === ids.pNotes)!, created_at: 150, updated_at: 400 };
  const run = runs(now)[0]!;
  const state = {
    ...emptyHostState,
    projects: Object.fromEntries(projects(now).map(p => [p.project_id, { ...p, created_at: 0 }])),
    tasks: { [tide.task_id]: tide, [notes.task_id]: notes },
    runs: {
      a: { ...run, run_id: "a", task_id: tide.task_id, admitted_at: 200, ended_at: null },
      b: { ...run, run_id: "b", task_id: notes.task_id, admitted_at: 300, ended_at: 5_000 },
    },
  };
  // Tide's agent has written status lines since; Notes finished later than Tide started.
  expect(projectCounts(state)[ids.pTide]?.moved).toBe(200);
  expect(projectCounts(state)[ids.pNotes]?.moved).toBe(5_000);
  const asks: AttentionItem = { attention_id: "q", kind: "question", task_id: tide.task_id, run_id: "a", title: "Which one?", body: null, approval_id: null, priority: 2, created_at: 6_000, resolved_at: null };
  expect(projectCounts({ ...state, attention: { q: asks } })[ids.pTide]?.moved).toBe(6_000);
});
