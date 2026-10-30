// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SimHost } from "./host";
import { ids } from "./scenario";

let host: SimHost;
beforeEach(() => { vi.useFakeTimers(); host = new SimHost(); });
afterEach(() => { host.dispose(); vi.useRealTimers(); });

const tide = () => host.checklists.lists.get(ids.pTide)!;
const notes = () => host.checklists.lists.get(ids.pNotes)!;
const item = (project: ReturnType<typeof tide>, id: string) => project.sections.flatMap((s) => s.items).find((entry) => entry.item_id === id)!;

it("exposes agent ownership and counts only the person's items in summaries", async () => {
  const result = await host.checklists.query({ kind: "checklists" });
  expect(result.kind).toBe("checklists");
  if (result.kind !== "checklists") return;
  expect(result.checklists).toHaveLength(2);
  const tideSummary = result.checklists.find((entry) => entry.project_id === ids.pTide)!;
  expect(tideSummary).toMatchObject({ open: 4, done: 1, for_agent: 1, waiting: 1, blockers: 1, next: "Give the agent a Stripe test key" });
  expect(item(tide(), "webhook-test")).toMatchObject({ owner: "agent", after: ["stripe-key"] });
  expect(host.host.capabilities.features).toContain("checklist_agent_items_v1");
  expect(item(tide(), "stripe-key")).toMatchObject({ task_id: ids.tHeaders, waiting: "Sets up the Stripe webhook and runs a test payment" });
  expect(host.tasks.find((task) => task.task_id === ids.tHeaders)).toMatchObject({ project_id: ids.pTide, state: "completed" });
  expect(host.queue.items.find((entry) => entry.task_id === ids.tReadme)).toMatchObject({ state: { state: "awaiting_start" } });
  expect(host.tasks.find((task) => task.task_id === ids.tReadme)?.current_run).toBeNull();
});

it("refuses any tick of agent work but accepts feedback", async () => {
  await expect(host.checklists.act({ command: "checklist_tick", project_id: ids.pTide, item_id: "webhook-test", done: true }, null))
    .rejects.toMatchObject({ code: "invalid_request", message: "“Test the Stripe webhook after access is ready” is the agent's to do. Send it feedback instead." });
  await expect(host.checklists.act({ command: "checklist_tick", project_id: ids.pTide, item_id: "webhook-test", step_id: "checkout", done: true }, null))
    .rejects.toMatchObject({ code: "invalid_request" });
  await host.checklists.act({ command: "checklist_ask", project_id: ids.pTide, item_id: "webhook-test", text: "Please check retries too" }, null);
  expect(item(tide(), "webhook-test").notes?.at(-1)).toMatchObject({ from: "person", text: "Please check retries too" });
});

it("finishes ready agent work shortly after the last person step, but cancels when unticked", async () => {
  await host.checklists.ready;
  for (const step_id of ["create", "save"]) await host.checklists.act({ command: "checklist_tick", project_id: ids.pTide, item_id: "stripe-key", step_id, done: true }, null);
  expect(item(tide(), "stripe-key").done_at).toBeDefined();
  expect(item(tide(), "webhook-test").done_at).toBeUndefined();
  await host.checklists.act({ command: "checklist_tick", project_id: ids.pTide, item_id: "stripe-key", done: false }, null);
  await vi.advanceTimersByTimeAsync(900);
  expect(item(tide(), "webhook-test").done_at).toBeUndefined();
  await host.checklists.act({ command: "checklist_tick", project_id: ids.pTide, item_id: "stripe-key", done: true }, null);
  await vi.advanceTimersByTimeAsync(900);
  expect(item(tide(), "webhook-test").steps[0]?.done_at).toBeDefined();
  const summary = await host.checklists.query({ kind: "checklists" });
  if (summary.kind !== "checklists") throw Error("wrong result");
  expect(summary.checklists.find((entry) => entry.project_id === ids.pTide)).toMatchObject({ for_agent: 0, done: 2 });
});

it("lets another project's person prerequisite release its own agent work", async () => {
  await host.checklists.act({ command: "checklist_tick", project_id: ids.pNotes, item_id: "review-tags", done: true }, null);
  await vi.advanceTimersByTimeAsync(900);
  expect(item(notes(), "apply-tags").done_at).toBeDefined();
  expect(item(tide(), "webhook-test").done_at).toBeUndefined();
});
