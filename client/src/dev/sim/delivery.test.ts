// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Command, TimelineItem } from "../../protocol/types";
import { rows } from "../../screens/talk/timeline";
import { SimHost } from "./host";

let host: SimHost;
let commandSeq = 0;
beforeEach(() => { vi.useFakeTimers(); host = new SimHost(); });
afterEach(() => { host.dispose(); vi.useRealTimers(); });
const send = (body: Command) => host.command({ command_id: `delivery-${++commandSeq}`, issued_at: Date.now(), expires_at: null, expected_revision: "task_id" in body ? host.tasks.find(t => t.task_id === body.task_id)?.revision ?? null : null, body });
function items(taskId: string): TimelineItem[] {
  return host.journal.flatMap(({ event }) => event.type === "timeline_appended" && event.task_id === taskId ? [event.item] : []);
}
function activeTask() {
  return host.tasks.find((task) => task.state === "working" && task.harness === "claude_code")!;
}

it("takes a next-turn message through accepted and delivered before replying", async () => {
  const task = activeTask();
  const result = await send({ command: "send_message", task_id: task.task_id, text: "After this turn", bundle: null, delivery: "next_turn" });
  expect(result.status).toBe("accepted");
  const message = items(task.task_id).at(-1)!;
  expect(message.item).toMatchObject({ kind: "user_message", delivery: "queued_for_boundary" });
  await vi.advanceTimersByTimeAsync(600);
  const updates = items(task.task_id).filter((i) => i.item.kind === "delivery" && i.item.message_seq === message.seq);
  expect(updates.map((i) => i.item)).toEqual([
    { kind: "delivery", message_seq: message.seq, state: "host_accepted" },
    { kind: "delivery", message_seq: message.seq, state: "native_delivered" },
  ]);
  await vi.advanceTimersByTimeAsync(5000);
  const reply = items(task.task_id).find((i) => i.seq > message.seq && i.item.kind === "agent_message");
  expect(reply?.seq).toBeGreaterThan(updates.at(-1)!.seq);
  expect(rows(items(task.task_id)).find((r) => r.key === `s${message.seq}`)).toMatchObject({ kind: "user", delivery: "native_delivered" });
});

it("holds a next-turn message while the turn waits for an approval, then delivers it once answered", async () => {
  const { ids } = await import("./scenario");
  const approval = host.approvals.find((a) => a.approval_id === ids.aTest)!;
  expect(host.runs.find((r) => r.run_id === approval.run_id)?.state).toBe("awaiting_approval");
  await send({ command: "send_message", task_id: approval.task_id, text: "Also check the clock", bundle: null, delivery: "next_turn" });
  const message = items(approval.task_id).at(-1)!;
  const deliveries = () => items(approval.task_id).flatMap((i) => i.item.kind === "delivery" && i.item.message_seq === message.seq ? [i.item.state] : []);
  await vi.advanceTimersByTimeAsync(5000);
  expect(deliveries()).toEqual([]);
  expect(rows(items(approval.task_id)).find((r) => r.key === `s${message.seq}`)).toMatchObject({ delivery: "queued_for_boundary" });
  await host.command({ command_id: "delivery-allow", issued_at: Date.now(), expires_at: null, expected_revision: approval.revision, body: { command: "resolve_approval", approval_id: approval.approval_id, decision: { decision: "allow" } } });
  await vi.advanceTimersByTimeAsync(600);
  expect(deliveries()).toEqual(["host_accepted", "native_delivered"]);
});

it("delivers an immediately accepted steer", async () => {
  const task = activeTask();
  expect((await send({ command: "send_message", task_id: task.task_id, text: "Adjust", bundle: null, delivery: "steer" })).status).toBe("accepted");
  const message = items(task.task_id).at(-1)!;
  expect(message.item).toMatchObject({ delivery: "host_accepted" });
  await vi.advanceTimersByTimeAsync(600);
  expect(rows(items(task.task_id)).find((r) => r.key === `s${message.seq}`)).toMatchObject({ delivery: "native_delivered" });
});

it.each([false, true])("Stop withdraws every held message and never sends one (force: %s)", async (force) => {
  const task = activeTask();
  await send({ command: "send_message", task_id: task.task_id, text: "Later", bundle: null, delivery: "next_turn" });
  await send({ command: "send_message", task_id: task.task_id, text: "Much later", bundle: null, delivery: "next_turn" });
  const held = items(task.task_id).slice(-2).map((i) => i.seq);
  expect((await send({ command: "stop_task", task_id: task.task_id, force })).status).toBe("accepted");
  // Withdrawn at once, as the companion's adapters report it, before the run's end.
  expect(items(task.task_id).filter((i) => i.item.kind === "delivery").slice(-2).map((i) => i.item))
    .toEqual(held.map((message_seq) => ({ kind: "delivery", message_seq, state: "cancelled", withdrawn_by_stop: true })));
  await vi.advanceTimersByTimeAsync(10_000);
  expect(items(task.task_id).filter((i) => i.item.kind === "delivery" && held.includes(i.item.message_seq))).toHaveLength(2);
  const talk = rows(items(task.task_id));
  expect(held.map((seq) => talk.find((r) => r.key === `s${seq}`))).toMatchObject([{ delivery: "cancelled", withdrawn: true }, { delivery: "cancelled", withdrawn: true }]);
  expect(talk.find((r) => r.kind === "run_ended")).toMatchObject({ state: "interrupted", withdrawn: 2 });
});

it("still reports an explicitly cancelled message as the person's own withdrawal", async () => {
  const task = activeTask();
  await send({ command: "send_message", task_id: task.task_id, text: "Never mind", bundle: null, delivery: "next_turn" });
  const message = items(task.task_id).at(-1)!;
  expect((await send({ command: "cancel_message", task_id: task.task_id, message_seq: message.seq })).status).toBe("accepted");
  await send({ command: "stop_task", task_id: task.task_id });
  await vi.advanceTimersByTimeAsync(1500);
  expect(items(task.task_id).filter((i) => i.item.kind === "delivery" && i.item.message_seq === message.seq).map((i) => i.item))
    .toEqual([{ kind: "delivery", message_seq: message.seq, state: "cancelled" }]);
  expect(rows(items(task.task_id)).find((r) => r.key === `s${message.seq}`)).toMatchObject({ delivery: "cancelled", withdrawn: false });
});

it("confirms a new prompt and a continuation only when their run starts", async () => {
  const template = activeTask();
  const receipt = await send({ command: "create_task", project_id: template.project_id, kind: "coding", mode: "ask", title: "Delivery check", prompt: "Check this", bundle: null, harness: template.harness, model: null, effort: null, mission: null, target_id: null, placement: { mode: "start_now" } });
  if (receipt.result?.kind !== "task_created") throw Error("Task was not created");
  const taskId = receipt.result.task_id;
  const first = items(taskId).find(i => i.item.kind === "user_message")!;
  expect(rows(items(taskId)).find(r => r.key === `s${first.seq}`)).toMatchObject({ delivery: "host_accepted" });
  await vi.advanceTimersByTimeAsync(600);
  expect(rows(items(taskId)).find(r => r.key === `s${first.seq}`)).toMatchObject({ delivery: "native_delivered" });
  await vi.advanceTimersByTimeAsync(30_000);
  expect((await send({ command: "resume_task", task_id: taskId, text: "Continue" })).status).toBe("accepted");
  const next = items(taskId).findLast(i => i.item.kind === "user_message")!;
  await vi.advanceTimersByTimeAsync(600);
  expect(rows(items(taskId)).find(r => r.key === `s${next.seq}`)).toMatchObject({ delivery: "native_delivered" });
});

it("denial returns control to the agent and ends with a result instead of a phantom question", async () => {
  const a = host.approvals.find(a => a.state === "pending")!;
  const result = await host.command({ command_id: `deny-${++commandSeq}`, issued_at: Date.now(), expires_at: null, expected_revision: a.revision,
    body: { command: "resolve_approval", approval_id: a.approval_id, decision: { decision: "deny", reason: null } } });
  expect(result.status).toBe("accepted");
  expect(host.tasks.find(t => t.task_id === a.task_id)?.state).toBe("working");
  expect(host.attention.filter(i => i.approval_id === a.approval_id && i.resolved_at === null)).toHaveLength(0);
  await vi.advanceTimersByTimeAsync(30_000);
  expect(host.tasks.find(t => t.task_id === a.task_id)?.state).toBe("completed");
  expect(host.attention.filter(i => i.task_id === a.task_id && i.resolved_at === null).map(i => i.kind)).toEqual(["completed"]);
});

it.each(["start_now", "queue", "continue"] as const)("settles an annotated %s prompt and its preview, including an early stop", async path => {
  const { ids } = await import("./scenario");
  const { applyDeliveries } = await import("../../state/timeline");
  const { emptyHostState } = await import("../../state/projection");
  const { resolvePreviewDelivery, previewDeliveryText } = await import("../../screens/control/previewDelivery");
  for (const stopEarly of [false, true]) {
    const bundle = { bundle_id: `bundle-${path}-${stopEarly}`, revision: 1, sources: [], marks: [], comments: [], steps: [], general_note: "Picture request", speech: null };
    const created = await send({ command: "create_task", project_id: ids.pNotes, kind: "coding", mode: "ask", title: "Picture start", prompt: "Picture request", bundle: path === "continue" ? null : bundle,
      harness: "claude_code", model: null, effort: null, mission: null, target_id: null,
      placement: path === "queue" ? { mode: "queue", position: null, authorization: "ask_before_start" } : { mode: "start_now" } });
    if (created.result?.kind !== "task_created") throw Error(JSON.stringify(created));
    const taskId = created.result.task_id;
    if (path === "queue") {
      const queue = host.talk.queue(ids.pNotes); queue.paused = false; host.talk.put(queue);
      expect((await send({ command: "resume_task", task_id: taskId, text: null })).status).toBe("accepted");
    } else if (path === "continue") {
      await vi.advanceTimersByTimeAsync(30_000);
      expect((await send({ command: "resume_task", task_id: taskId, text: "Picture continuation", bundle })).status).toBe("accepted");
    }
    const message = items(taskId).findLast(i => i.item.kind === "user_message" && i.item.bundle?.bundle_id === bundle.bundle_id)!;
    const runId = host.tasks.find(t => t.task_id === taskId)!.current_run;
    if (stopEarly) expect((await send({ command: "stop_task", task_id: taskId })).status).toBe("accepted");
    await vi.advanceTimersByTimeAsync(1500);
    const settlement = items(taskId).filter(i => i.item.kind === "delivery" && i.item.message_seq === message.seq);
    expect(settlement).toHaveLength(1);
    expect(settlement[0]).toMatchObject({ run_id: runId, item: { state: stopEarly ? "unknown" : "native_delivered" } });
    const task = host.tasks.find(t => t.task_id === taskId)!;
    const state = { ...emptyHostState, tasks: { [taskId]: task }, timelines: { [taskId]: { loaded: true, hasMore: false, items: applyDeliveries(items(taskId)) } } };
    const preview = resolvePreviewDelivery({ taskId, taskTitle: task.title, at: 1, bundleId: bundle.bundle_id, revision: 1, state: "host_accepted" }, state);
    expect(preview.state).toBe(stopEarly ? "unknown" : "native_delivered");
    expect(previewDeliveryText(preview, state)).toContain(stopEarly ? "is unconfirmed" : "Sent with markup");
  }
});
