// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Command, Run } from "../../protocol/types";
import { SimHost } from "./host";
import { ids } from "./scenario";

let host: SimHost;
let index = 0;
beforeEach(() => { vi.useFakeTimers(); host = new SimHost({ cards: true }); });
afterEach(() => { host.dispose(); vi.useRealTimers(); });
const send = (body: Command) => host.command({ command_id: `switch-${++index}`, issued_at: Date.now(), expires_at: null,
  expected_revision: "task_id" in body ? host.tasks.find(t => t.task_id === body.task_id)?.revision ?? null : null, body });
const current = () => host.runs.find(r => r.run_id === host.tasks.find(t => t.task_id === ids.tCards)?.current_run)!;
const messages = () => host.journal.flatMap(e => e.event.type === "timeline_appended" && e.event.task_id === ids.tCards ? [e.event.item] : []);
const handover = (run: Run) => run.chat_handover;

it("switches a completed conversation to another agent and resumes the earlier session on return", async () => {
  const first = await send({ command: "resume_task", task_id: ids.tCards, text: "Check the labels.", harness: "codex", model: "gpt-6-sol", effort: "low" });
  expect(first.status).toBe("accepted");
  const codex = current();
  expect(codex).toMatchObject({ harness: "codex", requested_model: "gpt-6-sol", requested_effort: "low" });
  expect(handover(codex)).toMatchObject({ after_seq: null, resumed: false });
  await vi.advanceTimersByTimeAsync(30_000);
  expect(current().state).toBe("completed");
  const back = await send({ command: "resume_task", task_id: ids.tCards, text: "Now compare them with the first answer.", harness: "claude_code", model: "opus", effort: "high" });
  expect(back.status).toBe("accepted");
  expect(current().harness).toBe("claude_code");
  expect(handover(current())).toMatchObject({ resumed: true, through_seq: messages().findLast(m => m.item.kind === "user_message")?.seq });
  expect(handover(current())?.after_seq).toBeGreaterThan(0);
  expect(current().native_session).toBeNull();
  await vi.advanceTimersByTimeAsync(500);
  expect(current().native_session).toBe(host.runs.find(r => r.run_id === ids.rCards)?.native_session);
});

it("keeps a switch message queued until the first turn ends and starts the chosen agent", async () => {
  await send({ command: "resume_task", task_id: ids.tCards, text: "Read the sample." });
  await vi.advanceTimersByTimeAsync(500);
  const former = current().run_id;
  const receipt = await send({ command: "send_message", task_id: ids.tCards, text: "Use another agent for the comparison.", bundle: null,
    delivery: "next_turn", harness: "codex", model: "gpt-6-sol", effort: "low" });
  expect(receipt.status).toBe("accepted");
  const queued = messages().findLast(m => m.item.kind === "user_message")!;
  expect(queued.item).toMatchObject({ delivery: "queued_for_boundary" });
  expect(current().run_id).toBe(former);
  await vi.advanceTimersByTimeAsync(30_000);
  expect(current()).toMatchObject({ harness: "codex", state: "completed" });
  expect(handover(host.runs.find(r => r.harness === "codex" && r.task_id === ids.tCards)!)).toMatchObject({ through_seq: queued.seq, resumed: false });
  expect(messages().flatMap(m => m.item.kind === "delivery" && m.item.message_seq === queued.seq ? [m.item.state] : [])).toEqual(["native_delivered"]);
});

it("uses the receiving agent’s defaults when switching without model or effort choices", async () => {
  const first = await send({ command: "resume_task", task_id: ids.tCards, text: "Check the labels.", harness: "codex" });
  expect(first.status).toBe("accepted");
  expect(host.tasks.find(t => t.task_id === ids.tCards)).toMatchObject({ harness: "codex", model: null, effort: null });
  expect(current()).toMatchObject({ harness: "codex", requested_model: null, requested_effort: null });
  await vi.advanceTimersByTimeAsync(500);
  expect(current()).toMatchObject({ actual_model: "gpt-6-astra", actual_effort: "high" });
});

it("uses the receiving agent’s defaults for a switch held until the next turn", async () => {
  await send({ command: "resume_task", task_id: ids.tCards, text: "Read the sample." });
  await vi.advanceTimersByTimeAsync(500);
  expect((await send({ command: "send_message", task_id: ids.tCards, text: "Compare with Codex.", bundle: null,
    delivery: "next_turn", harness: "codex" })).status).toBe("accepted");
  await vi.advanceTimersByTimeAsync(30_000);
  expect(host.tasks.find(t => t.task_id === ids.tCards)).toMatchObject({ harness: "codex", model: null, effort: null });
  expect(current()).toMatchObject({ harness: "codex", requested_model: null, actual_model: "gpt-6-astra", requested_effort: null, actual_effort: "high" });
});

it("refuses a mid-turn switch and cancels a held switch without starting the new agent", async () => {
  await send({ command: "resume_task", task_id: ids.tCards, text: "Read the sample." });
  await vi.advanceTimersByTimeAsync(500);
  const bad = await send({ command: "send_message", task_id: ids.tCards, text: "Switch now", bundle: null, delivery: "steer", harness: "codex" });
  expect(bad.status).toBe("rejected");
  await send({ command: "send_message", task_id: ids.tCards, text: "Switch later", bundle: null, delivery: "next_turn", harness: "codex" });
  const queued = messages().at(-1)!;
  expect((await send({ command: "cancel_message", task_id: ids.tCards, message_seq: queued.seq })).status).toBe("accepted");
  await vi.advanceTimersByTimeAsync(30_000);
  expect(current().harness).toBe("claude_code");
});
