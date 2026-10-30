// SPDX-License-Identifier: Apache-2.0
//
// A pending approval too large to share one frame with the rest of a reply is
// named instead, and the phone reads it alone. Whatever reads the reply finds
// it there, as if it had been carried.
import { afterEach, beforeEach, expect, it } from "vitest";
import { allGrants } from "../dev/sim/host";
import * as fx from "../dev/sim/scenario";
import { snapshot } from "../platform/lifecycle.fixtures";
import type { Approval, AttentionItem, ClientFrame, ErrorCode, Query, QueryData, ServerFrame } from "../protocol/types";
import type { ControlLink, LinkEvent } from "./link";
import { CommandError, HostSession } from "./session";

class ScriptedLink implements ControlLink {
  sent: ClientFrame[] = [];
  #listener: ((e: LinkEvent) => void) | undefined;
  #answered = new Set<string>();
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) {
    this.sent.push(frame);
    return true;
  }
  listen(listener: (e: LinkEvent) => void) {
    this.#listener = listener;
    return () => { this.#listener = undefined; };
  }
  frame(frame: ServerFrame) {
    this.#listener?.({ type: "frame", frame });
  }
  /** The unanswered queries of this kind, oldest first. */
  asked(kind: Query["kind"]): Query[] {
    return this.sent.flatMap((f) => (f.type === "query" && f.query.kind === kind && !this.#answered.has(f.query_id) ? [f.query] : []));
  }
  #take(which: (q: Query) => boolean): string {
    const frame = this.sent.find((f) => f.type === "query" && which(f.query) && !this.#answered.has(f.query_id));
    if (frame?.type !== "query") throw new Error("No such query");
    this.#answered.add(frame.query_id);
    return frame.query_id;
  }
  answer(data: QueryData, which = (_: Query) => true) {
    const query_id = this.#take((q) => q.kind === data.kind && which(q));
    this.frame({ type: "query_result", query_id, result: { Ok: data } });
  }
  refuse(approvalId: string, code: ErrorCode) {
    const query_id = this.#take((q) => q.kind === "approval" && q.approval_id === approvalId);
    this.frame({ type: "query_result", query_id, result: { Err: { code, message: "Refused" } } });
  }
  authenticate(capabilities = fx.capabilities()) {
    const snap = snapshot();
    this.#listener?.({ type: "authenticated", challenge: { selected: { major: 1, minor: 0 }, host_id: snap.host.host_id, boot_epoch: fx.ids.boot, server_nonce: "fixture" },
      auth: { host_id: snap.host.host_id, device_id: fx.ids.device, grants: allGrants, capabilities, companion_version: "test", journal_head: 0 } });
  }
}

const flush = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };
const template = snapshot();
const carried = template.approvals.find((a) => a.state === "pending")!;
const named = (n: number): Approval => ({ ...carried, approval_id: `0192f7a1-0000-7000-8000-0000000aa00${n}`, title: `Large approval ${n}` });
const [one, two] = [named(1), named(2)];
const read = (approval: Approval) => link.answer({ kind: "approval", approval }, (q) => q.kind === "approval" && q.approval_id === approval.approval_id);

let link: ScriptedLink;
let session: HostSession;
beforeEach(() => {
  localStorage.clear();
  link = new ScriptedLink();
  session = new HostSession("withheld", link);
  session.start();
  link.authenticate();
});
afterEach(() => session.stop());

async function loaded(snap: QueryData & { kind: "snapshot" }) {
  link.answer(snap);
  await flush();
  link.frame({ type: "caught_up", head: snap.journal_head });
}

it("a snapshot's withheld approvals are read one at a time and held with the rest", async () => {
  await loaded({ kind: "snapshot", ...template, withheld_approvals: [one.approval_id, two.approval_id] });
  expect(link.asked("approval")).toEqual([{ kind: "approval", approval_id: one.approval_id }]);
  // The snapshot waits for them, so nothing shows without them.
  expect(session.state.get().approvals[carried.approval_id]).toBeUndefined();
  read(one);
  await flush();
  expect(link.asked("approval")).toEqual([{ kind: "approval", approval_id: two.approval_id }]);
  // Decided and gone since the snapshot was taken.
  link.refuse(two.approval_id, "not_found");
  await flush();
  const approvals = session.state.get().approvals;
  expect(approvals[carried.approval_id]).toEqual(carried);
  expect(approvals[one.approval_id]).toEqual(one);
  expect(approvals[two.approval_id]).toBeUndefined();
});

it("a task or run read fills in its withheld approvals, and fails rather than leave one out", async () => {
  await loaded({ kind: "snapshot", ...template });
  const task = template.tasks.find((t) => t.task_id === carried.task_id)!;
  const run = template.runs.find((r) => r.run_id === carried.run_id)!;

  const taskRead = session.loadTask(task.task_id);
  link.answer({ kind: "task", records: { tasks: [task], runs: [], approvals: [], attention: [], withheld_approvals: [one.approval_id] } });
  await flush();
  read(one);
  await taskRead;
  expect(session.state.get().approvals[one.approval_id]).toEqual(one);

  const runRead = session.loadRun(run.run_id);
  await flush();
  link.answer({ kind: "run", run, approvals: [], attention: [], withheld_approvals: [two.approval_id] });
  await flush();
  link.refuse(two.approval_id, "temporarily_unavailable");
  await expect(runRead).rejects.toBeInstanceOf(CommandError);
  expect(session.state.get().approvals[two.approval_id]).toBeUndefined();
});

it("many withheld approvals never have more than one read waiting on the computer", async () => {
  // The computer queues only so many replies for a slow connection before
  // closing it, and each of these replies is large.
  const many = Array.from({ length: 40 }, (_, n) => ({ ...carried, approval_id: `0192f7a1-0000-7000-8000-0000000ab${String(n).padStart(3, "0")}` }));
  await loaded({ kind: "snapshot", ...template, withheld_approvals: many.map((a) => a.approval_id) });
  // A task read meanwhile names its own; it waits its turn too.
  const task = template.tasks.find((t) => t.task_id === carried.task_id)!;
  const taskRead = session.loadTask(task.task_id);
  link.answer({ kind: "task", records: { tasks: [task], runs: [], approvals: [], attention: [], withheld_approvals: [one.approval_id] } });
  await flush();
  for (const approval of [...many, one]) {
    expect(link.asked("approval")).toEqual([{ kind: "approval", approval_id: approval.approval_id }]);
    read(approval);
    await flush();
  }
  await taskRead;
  expect(link.asked("approval")).toEqual([]);
  const approvals = session.state.get().approvals;
  expect(many.every((a) => approvals[a.approval_id])).toBe(true);
  expect(approvals[one.approval_id]).toEqual(one);
});

it("a notice acknowledged while its read waits for approvals stays acknowledged", async () => {
  await loaded({ kind: "snapshot", ...template });
  const task = template.tasks.find((t) => t.task_id === carried.task_id)!;
  const notice: AttentionItem = { attention_id: "0192f7a1-0000-7000-8000-0000000ac001", kind: "completed", title: "Finished",
    body: null, task_id: task.task_id, run_id: null, approval_id: null, created_at: 1, resolved_at: null, priority: 1 };
  const taskRead = session.loadTask(task.task_id);
  link.answer({ kind: "task", records: { tasks: [task], runs: [], approvals: [], attention: [notice], withheld_approvals: [one.approval_id] } });
  await flush();
  // Another phone acknowledges it after the computer answered, before this
  // phone holds the answer.
  link.frame({ type: "event", seq: 1, at: 5, source: { kind: "device", device_id: fx.ids.device }, provenance: "native",
    event: { type: "attention_resolved", attention_id: notice.attention_id } });
  read(one);
  await taskRead;
  expect(session.state.get().attention[notice.attention_id]).toEqual({ ...notice, resolved_at: 5 });
});

it("a phone asks to have approvals named only of a computer that reads history", async () => {
  expect(link.asked("snapshot")).toEqual([{ kind: "snapshot", reads_withheld: true }]);
  // An earlier computer knows nothing of naming them, and sends them all.
  const earlier = new ScriptedLink();
  const other = new HostSession("earlier", earlier);
  other.start();
  earlier.authenticate({ ...fx.capabilities(), features: [] });
  expect(earlier.asked("snapshot")).toEqual([{ kind: "snapshot" }]);
  other.stop();
});

it("many runs read at once never have more than one waiting on the computer", async () => {
  await loaded({ kind: "snapshot", ...template });
  const base = template.runs[0]!;
  const runs = Array.from({ length: 40 }, (_, n) => ({ ...base, run_id: `0192f7a1-0000-7000-8000-0000000ad${String(n).padStart(3, "0")}` }));
  const reads = runs.map((run) => session.loadRun(run.run_id));
  // Asking again for one being read shares its read.
  expect(session.loadRun(runs[0]!.run_id)).toBe(reads[0]);
  await flush();
  for (const [n, run] of runs.entries()) {
    expect(link.asked("run")).toEqual([{ kind: "run", run_id: run.run_id }]);
    expect(link.asked("approval")).toEqual([]);
    // The first names an approval of its own, read in its turn after the rest.
    link.answer({ kind: "run", run, approvals: [], attention: [], withheld_approvals: n === 0 ? [one.approval_id] : [] }, (q) => q.kind === "run" && q.run_id === run.run_id);
    await flush();
  }
  expect(link.asked("approval")).toEqual([{ kind: "approval", approval_id: one.approval_id }]);
  read(one);
  expect(await Promise.all(reads)).toEqual(runs);
  expect(session.state.get().approvals[one.approval_id]).toEqual(one);
});
