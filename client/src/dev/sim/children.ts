// SPDX-License-Identifier: Apache-2.0
// Deterministic native-child and outside-session evidence. No native processes.
import { blockingAttentionCount } from "./attention";
import type { SimHost } from "./host";
import type { Command, CommandResult, InterventionReceipt, ObservedSessions, ReceiptFact, Run } from "../../protocol/types";
import { ids } from "./scenario";
import { workIds } from "./work";

const fail = (code: string, message: string): never => { throw { code, message }; };
export class ChildrenSim {
  observed: ObservedSessions;
  ignore = new Set<string>();
  constructor(private readonly host: SimHost) {
    for (const run of host.runs) for (const child of run.children) child.stop = child.native_ref
      ? { state: "available" } : { state: "unsupported", reason: "This helper has no native task control. Stop it in the agent’s terminal on the computer." };
    const project = host.projects[0];
    const now = Date.now();
    this.observed = { checked_at: now, coverage: ["Claude Code and Codex processes and session metadata are checked. Hermes, OpenClaw and Pi are not checked."], error: null,
      sessions: [{ observed_id: "outside-fixture", revision: 1, harness: "claude_code", native_session: null, process_id: 4242,
        executable: "/usr/bin/claude", cwd: project?.host_path ?? "/workspace/checkout", project_id: project?.project_id ?? null,
        started_at: now - 120000, last_activity: null, observed_at: now, running: true, stop: { state: "available" } }] };
  }
  query(): ObservedSessions {
    if (this.observed.checked_at !== null && Date.now() - this.observed.checked_at >= 10_000 && !this.observed.error) {
      this.observed = { ...this.observed, checked_at: Date.now(), sessions: this.observed.sessions.map(s => ({ ...s, observed_at: Date.now(), revision: s.revision + 1 })) };
    }
    return structuredClone(this.observed);
  }
  handle(command: Command, expected: number | null): CommandResult | null {
    if (command.command === "check_helpers") {
      const run = this.host.runs.find(r => r.run_id === command.run_id) ?? fail("not_found", "Run not found");
      const task = this.host.tasks.find(t => t.task_id === run.task_id) ?? fail("not_found", "Task not found");
      if (expected !== task.revision) fail("stale_revision", "This task changed. Review its helpers again.");
      if (!run.ended_at || run.helpers_checked || !run.children.some(c => c.state === "unknown")) fail("conflict", "There are no unchecked helper unknowns in this ended run");
      run.helpers_checked = { at: Date.now(), by: { kind: "device", device_id: ids.device } };
      for (const item of this.host.attention.filter(a => a.run_id === run.run_id && a.child_id && a.resolved_at === null)) {
        item.resolved_at = Date.now();
        this.host.emit({ type: "attention_resolved", attention_id: item.attention_id });
      }
      task.revision += 1;
      task.updated_at = Date.now();
      task.attention_count = blockingAttentionCount(this.host.attention, task.task_id);
      this.host.emit({ type: "run_upserted", run: structuredClone(run) });
      this.host.emit({ type: "task_upserted", task: structuredClone(task) });
      return null;
    }
    if (command.command === "stop_child") {
      const run = this.host.runs.find(r => r.run_id === command.run_id) ?? fail("not_found", "Run not found");
      const task = this.host.tasks.find(t => t.task_id === run.task_id) ?? fail("not_found", "Task not found");
      if (expected !== task.revision) fail("stale_revision", "This task changed. Review its helper again.");
      const child = run.children.find(c => c.child_id === command.child_id) ?? fail("not_found", "Helper not found");
      if (child.state !== "active" && child.state !== "unknown") fail("already_resolved", "This helper has already ended");
      if (child.stop?.state !== "available") fail("unsupported", "Stop this helper in the agent’s terminal on the computer.");
      task.revision += 1;
      this.host.emit({ type: "task_upserted", task: structuredClone(task) });
      const facts = this.stopChildren(run, command.child_id);
      return this.receipt(task.task_id, facts);
    }
    if (command.command === "stop_observed_session") {
      if (!command.confirmed) fail("invalid_request", "Confirm the outside process Stop first");
      const row = this.observed.sessions.find(s => s.observed_id === command.observed_id) ?? fail("not_found", "Outside process not found");
      if (expected !== row.revision || Date.now() - row.observed_at > 30000) fail("stale_revision", "Refresh and review this process again");
      if (row.stop.state !== "available") fail("unsupported", "This outside process cannot be stopped here");
      row.revision += 1;
      const ignored = this.ignore.has(row.observed_id);
      row.running = ignored;
      if (!ignored) row.stop = { state: "unsupported", reason: "The process group has ended" };
      return this.receipt(null, [{ label: `Outside agent process group ${row.process_id}`, status: ignored ? "still_active" : "done",
        detail: ignored ? "The process group is still present after the termination signal. Check it on the computer." : "No live processes remain in the observed group. Detached work and earlier file changes are not undone." }]);
    }
    return null;
  }
  stopChildren(run: Run, childId?: string): ReceiptFact[] {
    const facts: ReceiptFact[] = [];
    for (const child of run.children.filter(c => (!childId || c.child_id === childId) && (c.state === "active" || c.state === "unknown"))) {
      const unavailable = child.stop?.state !== "available";
      const ignored = this.ignore.has(child.child_id);
      if (!unavailable && !ignored) { child.state = "stopped"; child.ended_at = Date.now(); }
      facts.push({ label: `Child: ${child.label}`, status: unavailable ? "unknown" : ignored ? "still_active" : "done",
        detail: unavailable ? "The agent cannot confirm this helper ended. Check it on the computer." : ignored ? "Claude Code still reports this task after Stop. Check it in its terminal on the computer." : "Claude Code reported this task stopped." });
    }
    this.host.emit({ type: "run_upserted", run: structuredClone(run) });
    return facts;
  }
  private receipt(taskId: string | null, facts: ReceiptFact[]): CommandResult {
    const receipt: InterventionReceipt = { kind: "stop_task", requested_at: Date.now(), facts,
      summary: facts.every(f => f.status === "done") ? "The reported work has ended" : "Some work is still running or unconfirmed" };
    this.host.emit({ type: "intervention_recorded", task_id: taskId, target_id: null, receipt });
    return { kind: "intervention", receipt };
  }
  background(ignore = true) {
    const task = this.host.tasks.find(t => t.task_id === workIds.receipt);
    const run = this.host.runs.find(r => r.run_id === workIds.run);
    if (!task || !run) return;
    task.title = "Check the checkout build";
    task.state = "working";
    task.attention_count = blockingAttentionCount(this.host.attention, task.task_id);
    task.revision += 1;
    run.state = "working";
    run.ended_at = null;
    run.children = ["Build preview", "Watch integration tests"].map((label, i) => ({ child_id: `${workIds.run}-background-${i}`, native_ref: `job-${i}`, label,
      state: "active", started_at: Date.now() - 100000, ended_at: null, provenance: "native", stop: { state: "available" } }));
    const stubborn = run.children[1];
    if (stubborn && ignore) this.ignore.add(stubborn.child_id);
    this.host.emit({ type: "task_upserted", task: structuredClone(task) });
    this.host.emit({ type: "run_upserted", run: structuredClone(run) });
  }
  endedHelper(lifetime: "in_process" | "separate_process") {
    const task = this.host.tasks.find(t => t.task_id === workIds.receipt)!;
    const run = this.host.runs.find(r => r.run_id === workIds.run)!;
    task.title = "Summarize the project";
    task.state = "completed"; task.revision += 1;
    run.state = "completed"; run.ended_at = Date.now(); run.holds_writer = false;
    run.helpers_checked = undefined;
    run.outcome = { summary: "The project summary is ready.", changed_files: [], checks: [], still_active: [], unknowns: [], usage: null };
    const inProcess = lifetime === "in_process";
    const reason = inProcess ? "Ended with the agent process" : "Open this task’s conversation in Claude Code on the computer and check the helper’s task details and output.";
    run.children = [{ child_id: `${run.run_id}-helper`, native_ref: "summary-helper", label: "Summarize the README", lifetime,
      state: inProcess ? "stopped" : "unknown", provenance: inProcess ? "observed" : "native", started_at: run.admitted_at,
      ended_at: inProcess ? Date.now() : null, stop: { state: inProcess ? "unsupported" : "temporarily_unavailable", reason } }];
    for (const item of this.host.attention.filter(a => a.task_id === task.task_id && a.resolved_at === null)) {
      item.resolved_at = Date.now(); this.host.emit({ type: "attention_resolved", attention_id: item.attention_id });
    }
    if (!inProcess) {
      const item = { attention_id: `${run.run_id}-helper-alert`, child_id: run.children[0]!.child_id,
        kind: "reconciliation_required" as const, title: `${task.title} · Helper 'Summarize the README' — state unknown`, body: reason,
        task_id: task.task_id, run_id: run.run_id, approval_id: null, created_at: Date.now(), resolved_at: null, priority: 5 };
      this.host.attention = this.host.attention.filter(a => a.attention_id !== item.attention_id);
      this.host.attention.push(item); this.host.emit({ type: "attention_upserted", item });
    }
    task.attention_count = blockingAttentionCount(this.host.attention, task.task_id);
    this.host.emit({ type: "run_upserted", run: structuredClone(run) });
    this.host.emit({ type: "task_upserted", task: structuredClone(task) });
    return task.task_id;
  }
  shadowedCodex() {
    const detail = "Found /usr/local/bin/codex (codex-cli 0.100.0): the app-server reports shell tools are disabled. A newer Codex exists at /opt/codex/bin/codex (codex-cli 0.153.2). Select that executable or correct PATH.";
    const codex = this.host.harnesses.find(h => h.kind === "codex");
    if (codex) codex.capabilities.start_run = { state: "temporarily_unavailable", reason: detail };
    this.host.readiness = { ...this.host.readiness, checked_at: Date.now(), checks: [...this.host.readiness.checks.filter(c => c.label !== "Codex start run"),
      { area: "accounts", label: "Codex start run", status: "failed", detail, remedy: "Choose the newer Codex executable on the computer, then check readiness again." }] };
    this.host.emit({ type: "readiness_reported", report: this.host.readiness });
    this.host.emit({ type: "harnesses_changed", harnesses: this.host.harnesses });
  }
}
