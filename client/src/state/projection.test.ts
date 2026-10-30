// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { tasks, approvals, runs, targets, control, capabilities, hostInfo } from "../dev/sim/scenario";
import { applyEvent, applyLive, emptyHostState, openAttention, pendingApprovals, reviewAttention, unresolvedAttention, type HostState } from "./projection";

it.each(["Waiting for your reply", "Choose an agent to start", "Question waiting"])("exposes %s once in Needs you and its badge, until answered", status_line => {
  const task = { ...tasks(1000)[0]!, state: "awaiting_decision" as const, status_line };
  const s: HostState = { ...emptyHostState, tasks: { [task.task_id]: task } };
  expect(unresolvedAttention(s)).toHaveLength(1);
  expect(openAttention(s)).toHaveLength(1); // BottomNav uses this exact selector.
  expect(openAttention(s)[0]).toMatchObject({ task_id: task.task_id, kind: "waiting_for_you", body: status_line });
  const item = { ...openAttention(s)[0]!, attention_id: "native-waiting" };
  expect(openAttention({ ...s, attention: { [item.attention_id]: item } })).toHaveLength(1);
  expect(openAttention({ ...s, tasks: { [task.task_id]: { ...task, state: "working" } } })).toHaveLength(0);
  const approval = { ...approvals(1000)[0]!, task_id: task.task_id, kind: "question" as const };
  const attention = { ...item, attention_id: "native-question", kind: "approval" as const, approval_id: approval.approval_id };
  expect(openAttention({ ...s, approvals: { [approval.approval_id]: approval }, attention: { [attention.attention_id]: attention } })).toEqual([attention]);
});

it("keeps two helper alerts as two named rows and a badge of two across task renames", async () => {
  const { runs } = await import("../dev/sim/scenario");
  const { attentionGroups } = await import("../screens/work/attention");
  const task = { ...tasks(1000)[0]!, state: "completed" as const };
  const run = { ...runs(1000)[0]!, run_id: task.current_run!, task_id: task.task_id,
    children: ["indexer", "checker"].map(label => ({ child_id: label, label, state: "unknown" as const, started_at: 100, ended_at: null, native_ref: null, provenance: "native" as const })) };
  const attention = Object.fromEntries(run.children.map(c => [c.child_id, { attention_id: c.child_id, child_id: c.child_id,
    task_id: task.task_id, run_id: run.run_id, kind: "reconciliation_required" as const, title: `Helper outcome unknown · ${c.label}`,
    body: null, priority: 70, created_at: 1000, resolved_at: null, approval_id: null }]));
  for (const title of [task.title, "Renamed task"]) {
    const s: HostState = { ...emptyHostState, tasks: { [task.task_id]: { ...task, title } }, runs: { [run.run_id]: run }, attention };
    const items = openAttention(s);
    expect(items).toHaveLength(2);
    expect(attentionGroups(items)).toHaveLength(2);
    for (const child of run.children) expect(items.find(a => a.child_id === child.child_id)?.title).toBe(`${title} · helper ‘${child.label}’ · Outcome unknown`);
  }
});

it.each(["human", "agent"] as const)("counts input paused under a %s lease exactly once, until handed back or ended", owner => {
  const task = { ...tasks(1000)[0]!, state: "working" as const, status_line: "Working" };
  const target = targets()[0]!;
  task.target_id = target.target_id;
  const run = { ...runs(1000)[0]!, task_id: task.task_id, run_id: task.current_run!, state: "awaiting_input" as const, ended_at: null };
  const [view] = control(1000);
  const s: HostState = { ...emptyHostState, tasks: { [task.task_id]: task }, runs: { [run.run_id]: run }, targets: [target],
    control: { [target.target_id]: { ...view!, target_id: target.target_id, input_state: "paused", private_step: null,
      lease: { ...view!.lease!, state: "revoked", owner: owner === "human" ? { kind: "human", device_id: "phone" } : { kind: "agent", run_id: run.run_id } },
      model_observation: { ...view!.model_observation!, run_id: run.run_id } } } };
  expect(openAttention(s)).toHaveLength(1);
  expect(openAttention(s)[0]?.body).toBe(`Agent paused on ${target.label} · Resume agent`);
  const item = { ...openAttention(s)[0]!, attention_id: "native-waiting" };
  expect(openAttention({ ...s, attention: { [item.attention_id]: item } })).toHaveLength(1);
  expect(openAttention({ ...s, control: { [target.target_id]: { ...s.control[target.target_id]!, input_state: "agent" } } })).toHaveLength(0);
  expect(openAttention({ ...s, runs: { [run.run_id]: { ...run, state: "completed", ended_at: 2000 } } })).toHaveLength(0);
  expect(openAttention({ ...s, control: { [target.target_id]: { ...s.control[target.target_id]!, input_state: "ready", lease: null } } })).toHaveLength(0);
});

it("works out attention once while only live text and the time of last contact change", () => {
  let reads = 0;
  const task = { ...tasks(1000)[0]!, state: "awaiting_decision" as const, current_run: null };
  const attention = new Proxy({}, { ownKeys: (target) => { reads++; return Reflect.ownKeys(target); } });
  let s: HostState = { ...emptyHostState, tasks: { [task.task_id]: task }, attention };
  expect(openAttention(s)).toHaveLength(1);
  expect(reviewAttention(s)).toHaveLength(0);
  s = applyLive(s, { kind: "agent_text_delta", task_id: task.task_id, run_id: "run", message_key: "m", text: "Checking" }, 2000);
  s = { ...s, lastHeardAt: 3000 };
  unresolvedAttention(s).length = 0;
  expect(unresolvedAttention(s)).toHaveLength(1);
  expect(reads).toBe(1);
  expect(openAttention({ ...s, tasks: { [task.task_id]: { ...task, state: "working" } } })).toHaveLength(0);
  expect(reads).toBe(2);
});

it("sorts the pending approvals once per change to the approvals, however many tasks ask", () => {
  let reads = 0;
  const first = approvals(1000)[0]!;
  const later = { ...first, approval_id: "later", requested_at: first.requested_at + 1 };
  const decided = { ...first, approval_id: "decided", state: "allowed" as const };
  const other = { ...first, approval_id: "other", task_id: "another task", requested_at: first.requested_at - 1 };
  const records = Object.fromEntries([later, first, decided, other].map((a) => [a.approval_id, a]));
  const s: HostState = { ...emptyHostState, approvals: new Proxy(records, { ownKeys: (target) => { reads++; return Reflect.ownKeys(target); } }) };
  for (let i = 0; i < 5; i++) expect(pendingApprovals(s, first.task_id)).toEqual([first, later]);
  pendingApprovals(s).length = 0;
  expect(pendingApprovals(s)).toEqual([other, first, later]);
  expect(pendingApprovals(s, "nobody")).toEqual([]);
  expect(reads).toBe(1);
});

it("lists a task's runs in the order of the runs record, as a fresh array", async () => {
  const { taskRuns } = await import("./projection");
  const base = runs(1000)[0]!;
  const record = Object.fromEntries(["c", "a", "x", "b"].map((id) => [id, { ...base, run_id: id, task_id: id === "x" ? "other" : base.task_id }]));
  const s: HostState = { ...emptyHostState, runs: record };
  expect(taskRuns(s, base.task_id)).toEqual(Object.values(record).filter((r) => r.task_id === base.task_id));
  taskRuns(s, base.task_id).length = 0;
  expect(taskRuns(s, base.task_id).map((r) => r.run_id)).toEqual(["c", "a", "b"]);
  expect(taskRuns(s, "none")).toEqual([]);
});

it("remembers the latest resolutions of attention it does not hold, and no more", async () => {
  const { applyEvent, applyTaskRecords } = await import("./projection");
  let s: HostState = emptyHostState;
  for (let seq = 1; seq <= 250; seq++)
    s = applyEvent(s, { seq, at: seq * 10, source: { kind: "companion" }, provenance: "native", event: { type: "attention_resolved", attention_id: `notice-${seq}` } }, 0);
  expect(Object.keys(s.resolvedAttention)).toHaveLength(200);
  const notice = (n: number) => ({ attention_id: `notice-${n}`, kind: "completed" as const, title: "Finished", body: null,
    task_id: null, run_id: null, approval_id: null, created_at: 1, resolved_at: null, priority: 1 });
  const read = applyTaskRecords(s, { tasks: [], runs: [], approvals: [], attention: [notice(250), notice(50)] });
  expect(read.attention["notice-250"]?.resolved_at).toBe(2500);
  expect(read.attention["notice-50"]?.resolved_at).toBeNull();
});

it("forgets resolutions from a journal the computer has since rolled back", async () => {
  const { applyEvent, applyJournalReset, applySnapshot, applyTaskRecords } = await import("./projection");
  let s = applyEvent(emptyHostState, { seq: 9, at: 90, source: { kind: "companion" }, provenance: "native", event: { type: "attention_resolved", attention_id: "notice-1" } }, 0);
  // The computer restores a backup from before the resolution.
  s = applyJournalReset(s, 4, 100);
  s = applySnapshot(s, { journal_head: 4, host: s.host!, projects: [], tasks: [], runs: [], approvals: [], attention: [], targets: [], control: [], library_roots: [], withheld_approvals: [] }, 100);
  const notice = { attention_id: "notice-1", kind: "completed" as const, title: "Finished", body: null,
    task_id: null, run_id: null, approval_id: null, created_at: 1, resolved_at: null, priority: 1 };
  const read = applyTaskRecords(s, { tasks: [], runs: [], approvals: [], attention: [notice] });
  expect(read.attention["notice-1"]?.resolved_at).toBeNull();
});

it("shows what the computer can do as soon as its host record changes", () => {
  const off = { ...capabilities(), capture: { state: "permission_missing" as const, remedy: "Run ophio desktop enable" } };
  const before: HostState = { ...emptyHostState, journalHead: 4, capabilities: off };
  const host = { ...hostInfo(1000), capabilities: capabilities() };
  const after = applyEvent(before, { seq: 5, at: 1000, source: { kind: "companion" }, provenance: "native", event: { type: "host_updated", host } }, 1000);
  expect(after.capabilities?.capture).toEqual({ state: "available" });
  expect(after.host).toBe(host);
});

it("invalidates only a changed checklist project, and every checklist after a snapshot", async () => {
  const { applySnapshot, refreshFromSnapshot } = await import("./projection");
  const s = applyEvent(emptyHostState, { seq: 1, at: 10, source: { kind: "companion" }, provenance: "native", event: { type: "checklist_changed", project_id: "notes" } }, 10);
  expect(s.checklistsRevision).toBe(1);
  expect(s.checklistProjects).toEqual({ notes: 1 });
  expect(s.checklistEpoch).toBe(0);
  const next = applySnapshot(s, { journal_head: 1, host: hostInfo(20), projects: [], tasks: [], runs: [], approvals: [], attention: [], targets: [], control: [], library_roots: [], withheld_approvals: [] }, 20);
  expect(next.checklistEpoch).toBe(1);
  expect(next.checklistsRevision).toBe(2);
  const refreshed = refreshFromSnapshot(s, { journal_head: 1, host: hostInfo(20), projects: [], tasks: [], runs: [], approvals: [], attention: [], targets: [], control: [], library_roots: [], withheld_approvals: [] }, 20);
  expect(refreshed.checklistEpoch).toBe(s.checklistEpoch);
  expect(refreshed.checklistsRevision).toBe(s.checklistsRevision);
});
