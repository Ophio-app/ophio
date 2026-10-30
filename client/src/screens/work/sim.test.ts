// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { SimHost } from "../../dev/sim/host";
import { workIds } from "../../dev/sim/work";
import { ids } from "../../dev/sim/scenario";
import type { AnnotationBundle, Command } from "../../protocol/types";
const hosts: SimHost[] = [];
beforeEach(() => localStorage.clear());
const host = () => {
  const h = new SimHost();
  hosts.push(h);
  return h;
};
let id = 0;
const command = (h: SimHost, body: Command, revision = h.queue.revision) =>
  h.command({
    command_id: `test-${++id}`,
    issued_at: Date.now(),
    expires_at: null,
    expected_revision: revision,
    body,
  });
afterEach(() => hosts.splice(0).forEach((h) => h.dispose()));
describe("simulated queue authority", () => {
  it("revokes auto-start after a move and rejects stale edits", async () => {
    const h = host();
    const rev = h.queue.revision;
    expect(
      (
        await command(h, {
          command: "queue_move",
          item_id: workIds.qReady,
          position: 1,
        })
      ).status,
    ).toBe("accepted");
    expect(
      h.queue.items.find((i) => i.item_id === workIds.qWaiting)?.authorization,
    ).toBe("ask_before_start");
    const reply = await command(
      h,
      { command: "queue_remove", item_id: workIds.qReady },
      rev,
    );
    expect(reply.error?.code).toBe("stale_revision");
  });
  it("rejects cycles and archives a task removed before starting", async () => {
    const h = host();
    const cyclic = await command(h, {
      command: "queue_set_dependencies",
      item_id: workIds.qWaiting,
      depends_on: [workIds.qBlocked],
    });
    expect(cyclic.error?.message).toContain("cycle");
    const before = h.tasks.find((t) => t.task_id === workIds.ready);
    expect(
      (await command(h, { command: "queue_remove", item_id: workIds.qReady }))
        .status,
    ).toBe("accepted");
    expect(h.tasks.find((t) => t.task_id === workIds.ready)).toMatchObject({
      task_id: before?.task_id, state: "archived", revision: (before?.revision ?? 0) + 1, current_run: null,
      status_line: "Removed from Next up before starting",
    });
  });
  it("names the admitted run when pause loses the race", async () => {
    const h = host();
    h.work.admitNextBeforePause();
    await command(h, { command: "queue_pause", project_id: ids.pTide });
    expect(h.queue.paused).toBe(true);
    expect(
      h.queue.items.find((i) => i.item_id === workIds.qWaiting)?.state.state,
    ).toBe("started");
    const event = h.journal
      .filter((e) => e.event.type === "intervention_recorded")
      .at(-1)?.event;
    expect(event?.type === "intervention_recorded" && event.task_id).toBe(
      workIds.waiting,
    );
    expect(
      event?.type === "intervention_recorded" && event.receipt.summary,
    ).toContain("Polish empty states");
  });
  it("keeps pause separate from stop", async () => {
    const h = host();
    const before = h.runs.find((r) => r.run_id === ids.rFlaky)?.state;
    await command(h, { command: "queue_pause", project_id: ids.pTide });
    expect(h.runs.find((r) => r.run_id === ids.rFlaky)?.state).toBe(before);
  });
  it("refuses a decision on an approval resolved elsewhere", async () => {
    const h = host();
    h.work.resolveElsewhere(ids.aTest);
    const r = await command(
      h,
      {
        command: "resolve_approval",
        approval_id: ids.aTest,
        decision: { decision: "allow" },
      },
      1,
    );
    expect(r.error?.code).toBe("already_resolved");
    expect(h.approvals.find((a) => a.approval_id === ids.aTest)?.state).toBe(
      "resolved_elsewhere",
    );
  });
});

describe("simulated writer ownership", () => {
  const create = (h: SimHost, mode: "ask" | "work") =>
    command(
      h,
      {
        command: "create_task",
        project_id: ids.pTide,
        kind: "coding",
        mode,
        prompt: "What does the retry helper do?",
        title: null,
        harness: "claude_code",
        model: null,
        effort: null,
        placement: { mode: "start_now" },
        mission: null,
        bundle: null,
        target_id: null,
      } as Command,
    );

  it("starts Ask beside a busy writer without taking it, and still refuses Work", async () => {
    const h = host();
    const writer = h.projects.find((p) => p.project_id === ids.pTide)!.active_writer;
    expect(writer).toBe(ids.rFlaky);

    const ask = await create(h, "ask");
    expect(ask.status).toBe("accepted");
    const askTask = h.tasks.find((t) => t.mode === "ask" && t.title === "What does the retry helper do?");
    const askRun = h.runs.find((r) => r.task_id === askTask?.task_id);
    expect(askRun?.holds_writer).toBe(false);
    expect(h.projects.find((p) => p.project_id === ids.pTide)!.active_writer).toBe(writer);

    const work = await create(h, "work");
    expect(work.status).toBe("rejected");
    expect(work.error?.code).toBe("conflict");
  });
});
describe("simulated messages", () => {
  const bundle: AnnotationBundle = {
    bundle_id: "bundle",
    revision: 1,
    sources: [],
    marks: [],
    comments: [],
    steps: [],
    general_note: "Make this match the mockup",
    speech: null,
  };
  it("accepts an annotation alone and refuses a message with neither", async () => {
    const h = host();
    const send = (text: string, b: AnnotationBundle | null) =>
      command(h, {
        command: "send_message",
        task_id: ids.tDash,
        text,
        bundle: b,
        delivery: "next_turn",
      });
    expect((await send("", bundle)).status).toBe("accepted");
    expect((await send(" \n", null)).error?.code).toBe("invalid_request");
  });
});

it("recording failure resolves outcome-unknown attention", async () => {
  const h = host();
  const task = h.tasks.find(t => t.state === "reconciliation_required")!;
  await command(h, { command: "reconcile_task", task_id: task.task_id, resolution: "mark_failed" }, task.revision);
  expect(h.tasks.find(t => t.task_id === task.task_id)?.state).toBe("failed");
  expect(h.attention.filter(a => a.task_id === task.task_id && a.kind === "reconciliation_required" && !a.resolved_at)).toHaveLength(0);
});

describe("simulated blocker counts", () => {
  it("keeps completed and failed results for review without counting them as blockers", async () => {
    const h = host();
    for (const taskId of [ids.tHeaders, workIds.receipt, workIds.failed]) {
      expect(h.tasks.find(t => t.task_id === taskId)?.attention_count).toBe(0);
      expect(h.attention.some(a => a.task_id === taskId && a.resolved_at === null)).toBe(true);
    }
    expect(h.tasks.find(t => t.task_id === workIds.unknown)?.attention_count).toBe(1);
    const blocker = h.attention.find(a => a.task_id === workIds.unknown)!;
    await command(h, { command: "acknowledge_attention", attention_id: blocker.attention_id });
    expect(h.tasks.find(t => t.task_id === workIds.unknown)?.attention_count).toBe(0);
    h.work.resolveElsewhere(ids.aTest);
    expect(h.tasks.find(t => t.task_id === ids.tFlaky)?.attention_count).toBe(0);
  });
});
