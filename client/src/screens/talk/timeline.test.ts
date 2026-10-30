// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import type { TimelineItem } from "../../protocol/types";
import { keepUnchanged, rows, stepsSummary, type ActivityItem } from "./timeline";

let seq = 0;
const at = (body: TimelineItem["item"]): TimelineItem => ({ seq: ++seq, at: 1000 + seq, run_id: "r1", item: body }) as TimelineItem;
const step = (summary: string, status: "running" | "succeeded" | "failed" = "succeeded") => at({ kind: "activity", tool: "shell", summary, status, detail: null });

describe("rows", () => {
  it("folds consecutive steps and keeps words and run boundaries separate", () => {
    const out = rows([
      at({ kind: "user_message", text: "Fix it", bundle: null, delivery: "host_accepted" }),
      at({ kind: "run_started", run_id: "r1" }),
      step("Read a"),
      step("Edit b"),
      at({ kind: "agent_message", text: "Done", partial: false }),
      step("cargo test"),
      at({ kind: "run_ended", run_id: "r1", state: "completed" }),
    ]);
    expect(out.map((r) => r.kind)).toEqual(["user", "run_started", "steps", "agent", "steps", "run_ended"]);
    const first = out[2];
    expect(first?.kind === "steps" && first.steps.map((s) => s.item.summary)).toEqual(["Read a", "Edit b"]);
  });

  it("drops nothing: every activity lands in exactly one group", () => {
    const items = [step("a"), step("b"), at({ kind: "note", text: "n" }), step("c")];
    const grouped = rows(items).flatMap((r) => (r.kind === "steps" ? r.steps : []));
    expect(grouped).toHaveLength(3);
  });

  it("confirms a receipt only when every fact is confirmed", () => {
    const receipt = (...statuses: ("done" | "not_applicable" | "unknown" | "still_active" | "failed")[]) =>
      at({ kind: "receipt", receipt: { kind: "stop_task", requested_at: 1, summary: "Stop", facts: statuses.map((status) => ({ label: "x", status, detail: null })) } });
    const outcomes = rows([receipt("done", "not_applicable"), receipt("done", "unknown"), receipt("still_active", "failed"), receipt()]).map((r) => r.kind === "receipt" && r.outcome);
    expect(outcomes).toEqual(["confirmed", "open", "failed", "open"]);
  });
});

it("shows a tool call once, updated in place by its result", () => {
  const call = (key: string, summary: string, status: "running" | "succeeded" | "failed", detail: string | null) =>
    at({ kind: "activity", key, tool: "Bash", summary, status, detail });
  const start = call("t1", "Running Bash", "running", '{"command":"pwd"}');
  const out = rows([
    start,
    call("c1", "Running Task", "running", null),
    call("t1", "Native tool is running", "running", null),
    call("t1", "Bash finished", "succeeded", '"fixture"'),
    step("unkeyed"),
  ]);
  expect(out).toHaveLength(1);
  const group = out[0]!;
  if (group.kind !== "steps") throw new Error("expected steps");
  expect(group.steps.map((s) => [s.item.summary, s.item.status, s.item.detail, s.output ?? null])).toEqual([
    ["Bash finished", "succeeded", '{"command":"pwd"}', '"fixture"'],
    ["Running Task", "running", null, null],
    ["unkeyed", "succeeded", null, null],
  ]);
  expect(start.item).toMatchObject({ summary: "Running Bash", status: "running" });
});

describe("stepsSummary", () => {
  it("leads with the latest step and counts failures", () => {
    const steps = [step("a"), step("b", "failed"), step("c", "running")] as ActivityItem[];
    expect(stepsSummary(steps)).toEqual({ latest: "c", count: "3 steps", failed: 1, notRun: { denied: 0, stopped: 0 }, running: true });
    expect(stepsSummary([step("only")] as ActivityItem[]).count).toBe("1 step");
  });

  it("counts a call that never ran by why, not as failed, and says so in its line", () => {
    const call = (key: string, tool: string, status: "running" | "failed", not_run?: "denied" | "stopped") =>
      at({ kind: "activity", key, tool, summary: status === "running" ? tool : `${tool} finished`, status, detail: null, ...(not_run && { not_run }) });
    const out = rows([
      call("t1", "Bash", "running"), call("t1", "Bash", "failed", "denied"),
      call("t2", "Bash", "failed"),
      call("t3", "Edit", "running"), call("t3", "Edit", "failed", "stopped"),
    ]);
    const group = out[0]!;
    if (group.kind !== "steps") throw new Error("expected steps");
    expect(group.steps.map((s) => s.item.not_run)).toEqual(["denied", undefined, "stopped"]);
    expect(stepsSummary(group.steps, true)).toEqual({ latest: "Edit: Not run (stopped)", count: "3 steps", failed: 1, notRun: { denied: 1, stopped: 1 }, running: false });
  });
});

it("folds delivery into its user row and preserves the source journal", () => {
  const message = at({ kind: "user_message", text: "After this turn", bundle: null, delivery: "queued_for_boundary" });
  const reply = at({ kind: "agent_message", text: "I took the message", partial: false });
  const items = [message, at({ kind: "delivery", message_seq: message.seq, state: "host_accepted" }), reply, at({ kind: "delivery", message_seq: message.seq, state: "native_delivered" })];
  expect(rows(items)).toMatchObject([
    { kind: "user", key: `s${message.seq}`, delivery: "native_delivered" },
    { kind: "agent", text: "I took the message" },
  ]);
  expect(message.item).toMatchObject({ delivery: "queued_for_boundary" });
});

it("hides updates for messages outside the loaded page without breaking step groups", () => {
  const a = step("Read");
  const update = at({ kind: "delivery", message_seq: 0, state: "failed" });
  expect(rows([update])).toEqual([]);
  expect(rows([a, update, step("Check")])).toMatchObject([{ kind: "steps", steps: [{ item: { summary: "Read" } }, { item: { summary: "Check" } }] }]);
});

it("keeps the latest state of each message, including a failed queued message", () => {
  const a = at({ kind: "user_message", text: "first", bundle: null, delivery: "queued_for_boundary" });
  const b = at({ kind: "user_message", text: "second", bundle: null, delivery: "queued_for_boundary" });
  expect(rows([a, b, at({ kind: "delivery", message_seq: a.seq, state: "native_delivered" }), at({ kind: "delivery", message_seq: b.seq, state: "failed" })])).toMatchObject([
    { key: `s${a.seq}`, delivery: "native_delivered" },
    { key: `s${b.seq}`, delivery: "failed" },
  ]);
});

it("a step still marked running stops reading as running once its run has ended", () => {
  const open = rows([at({ kind: "run_started", run_id: "r1" }), step("pwd", "running")]);
  expect(open.find((r) => r.kind === "steps")).toMatchObject({ ended: false });
  const stopped = rows([at({ kind: "run_started", run_id: "r1" }), step("pwd", "running"), at({ kind: "run_ended", run_id: "r1", state: "interrupted" })]);
  expect(stopped.find((r) => r.kind === "steps")).toMatchObject({ ended: true });
});

it("groups late receipts with their old run and marks the new one before its request", () => {
  const old = at({ kind: "run_started", run_id: "r1" });
  const ended = at({ kind: "run_ended", run_id: "r1", state: "interrupted" });
  const next = { ...at({ kind: "user_message", text: "Try again", bundle: null, delivery: "host_accepted" }), run_id: "r2" };
  const receipt = at({ kind: "receipt", receipt: { kind: "take_over", requested_at: 1, summary: "You have control", facts: [] } });
  const started = { ...at({ kind: "run_started", run_id: "r2" }), run_id: "r2" };
  expect(rows([old, ended, next, receipt, started])).toMatchObject([
    { kind: "run_started", number: 1 }, { kind: "run_ended", runId: "r1" },
    { kind: "receipt", summary: "You have control" }, { kind: "run_started", number: 2 },
    { kind: "user", text: "Try again" },
  ]);
});
it("does not merge native tool keys across runs", () => {
  const first = { ...step("Old call", "running"), item: { kind: "activity" as const, key: "same", tool: "Bash", summary: "Old call", status: "running" as const, detail: null } };
  const second = { ...first, seq: first.seq + 1, run_id: "r2", item: { ...first.item, summary: "New call", status: "succeeded" as const } };
  expect(rows([first, second]).filter(r => r.kind === "steps").map(r => r.steps[0]?.item.summary)).toEqual(["Old call", "New call"]);
});

it("deduplicates replayed takeover receipts inside their original run", () => {
  const old = at({ kind: "run_started", run_id: "r1" });
  const next = { ...at({ kind: "user_message", text: "Continue", bundle: null, delivery: "host_accepted" }), run_id: "r2" };
  const receipt = at({ kind: "receipt", receipt: { kind: "take_over", requested_at: 1, summary: "You have control.", facts: [] } });
  const start = { ...at({ kind: "run_started", run_id: "r2" }), run_id: "r2" };
  const replayed = rows([old, next, receipt, { ...receipt, seq: receipt.seq + 100 }, start]);
  expect(replayed.filter(r => r.kind === "receipt")).toHaveLength(1);
  const controlIndex = replayed.findIndex(r => r.kind === "receipt");
  const continuationIndex = replayed.findIndex(r => r.kind === "run_started" && r.runId === "r2");
  expect(controlIndex).toBeLessThan(continuationIndex);
  expect(replayed[continuationIndex + 1]).toMatchObject({ kind: "user", text: "Continue" });
});

it("keeps older Running summaries readable when stopped without a final tool result", () => {
  const old = step("Running Task", "running") as ActivityItem;
  expect(stepsSummary([old]).latest).toBe("Running Task");
  expect(stepsSummary([old], true)).toMatchObject({ latest: "Task", running: false });
  expect(stepsSummary([{ ...old, item: { ...old.item, status: "failed" } }]).latest).toBe("Task");
  const ended = rows([old, at({ kind: "run_ended", run_id: "r1", state: "interrupted" })])[0];
  expect(ended?.kind === "steps" && stepsSummary(ended.steps, ended.ended).latest).toBe("Task");
});

 it("labels a native plan proposal as an action", () => {
   const item = at({ kind: "activity", tool: "ExitPlanMode", summary: "ExitPlanMode", status: "running", detail: '{"plan":"Inspect the parser"}' });
   expect(stepsSummary([item as ActivityItem], true).latest).toBe("Plan proposed");
 });

it("keeps the earlier object for each row a rebuild leaves unchanged", () => {
  const items = [at({ kind: "user_message", text: "Go", bundle: null, delivery: "host_accepted" }), step("Read a file", "running"), at({ kind: "agent_message", text: "Done", partial: false })];
  const before = rows(items);
  const after = keepUnchanged(before, rows([...items, at({ kind: "note", text: "Later" })]));
  expect(after.slice(0, 3)).toEqual(before);
  after.slice(0, 3).forEach((row, i) => expect(row).toBe(before[i]));
  const changed = keepUnchanged(after, rows([items[0]!, { ...items[1]!, item: { ...items[1]!.item, status: "succeeded" } } as TimelineItem, items[2]!]));
  expect(changed[0]).toBe(before[0]);
  expect(changed[1]).not.toBe(before[1]);
  expect(changed[1]).toMatchObject({ kind: "steps", steps: [{ item: { status: "succeeded" } }] });
  expect(changed[2]).toBe(before[2]);
});
