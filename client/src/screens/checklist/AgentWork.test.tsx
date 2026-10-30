// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import type { AttentionItem, Checklist, ChecklistItem, QueueItem, QueueView, Task } from "../../protocol/types";
import { emptyHostState } from "../../state/projection";
import { AgentWorkEntry, deriveAgentWork } from "./AgentWork";

afterEach(cleanup);

const task = (id: string, state: Task["state"] = "working"): Task => ({
  task_id: id, project_id: "project", title: id, state,
} as Task);
const item = (id: string, taskId: string, extra: Partial<ChecklistItem> = {}): ChecklistItem => ({
  item_id: id, title: id, task_id: taskId, priority: "high", steps: [], added_at: 1, changed_at: 1, ...extra,
});
const list = (...items: ChecklistItem[]): Checklist => ({
  project_id: "project", title: "To-Do", sections: [{ section_id: "first", title: "", items }], created_at: 1, updated_at: 1, revision: 1,
});
const state = (...tasks: Task[]) => ({ ...emptyHostState, tasks: Object.fromEntries(tasks.map(t => [t.task_id, t])) });

it("drops a checklist-blocked task on an optimistic person tick, including agent prerequisites", () => {
  const a = task("writer");
  const b = task("agent");
  const person = item("Sign in", a.task_id, { waiting: "Finish setup" });
  const prerequisite = item("Choose a color", b.task_id);
  const agent = item("Build theme", b.task_id, { owner: "agent", after: [prerequisite.item_id] });
  const before = list(person, prerequisite, agent);
  const host = state(a, b);
  expect(deriveAgentWork(host, "project", before).map(e => e.checklistItemId)).toEqual(["Sign in", "Choose a color"]);
  const pending = list({ ...person, done_at: 2 }, { ...prerequisite, done_at: 2 }, agent);
  expect(deriveAgentWork(host, "project", pending)).toEqual([]);
});

it("opens feedback in the existing task composer, but unstarted work in Next up", () => {
  const ui = new UiStore("test-profile");
  ui.setTodoAll(true);
  const app = { ui } as App;
  const waiting = deriveAgentWork(state(task("writer")), "project", list(item("Sign in", "writer", { waiting: "Continue" })))[0]!;
  const { rerender } = render(<AppProvider app={app}><ul><AgentWorkEntry entry={waiting} onChecklist={() => {}} /></ul></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Send feedback" }));
  expect(ui.get().tab).toBe("talk");
  expect(ui.get().taskByProject.project).toBe("writer");

  const queued = deriveAgentWork(state(task("next", "accepted")), "project", null,
    { project_id: "project", revision: 1, paused: false, writer: null, items: [{
      item_id: "q", task_id: "next", position: 0, state: { state: "waiting" }, depends_on: [],
      authorization: "ask_before_start", added_by: { kind: "local_owner" }, added_at: 1,
    }] })[0]!;
  rerender(<AppProvider app={app}><ul><AgentWorkEntry entry={queued} onChecklist={() => {}} /></ul></AppProvider>);
  expect(screen.queryByRole("button", { name: "Send feedback" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Open in Next up" }));
  expect(ui.get().tab).toBe("work");
  expect(ui.get().stacks.work.at(-1)).toEqual({ screen: "next_up" });
  expect(ui.get().projectId).toBe("project");
  expect(ui.get().todoAll).toBe(true);
});

it("matches Needs you decisions and suppresses duplicate Next up rows", () => {
  const a = task("writer", "accepted");
  const alert: AttentionItem = { attention_id: "ask", kind: "waiting_for_you", title: "Review start", body: null,
    task_id: a.task_id, run_id: null, approval_id: null, created_at: 2, resolved_at: null, priority: 90 };
  const queueItem: QueueItem = { item_id: "queued", task_id: a.task_id, position: 0, state: { state: "awaiting_start" }, depends_on: [], authorization: "ask_before_start", added_by: { kind: "local_owner" }, added_at: 1 };
  const queue = { project_id: "project", revision: 1, paused: false, writer: null, items: [queueItem] } as QueueView;
  const host = { ...state(a), attention: { ask: alert }, queues: { project: queue } };
  const entries = deriveAgentWork(host, "project", null, queue);
  expect(entries).toMatchObject([{ id: "attention:ask", tier: 0, kind: "attention", feedback: false }]);
  expect(deriveAgentWork({ ...host, attention: {} }, "project", null, queue)).toMatchObject([
    { id: "next:queued", tier: 2, kind: "next", detail: "Not started", feedback: false },
  ]);
});
