// SPDX-License-Identifier: Apache-2.0
// Simulated queue admission uses the same review, ordering and expiry guards.
import type { AnnotationBundle, Command, CommandResult, QueueAuthorization, QueueItem, QueueView, Task, TaskPlacement } from "../../protocol/types";
import { blockingAttentionCount } from "./attention";
import { uuidv7 } from "../../protocol/ids";
import { queueMoveError, waitingOn } from "../../screens/work/useQueue";
import type { SimHost } from "./host";

const fail = (code: string, message: string): never => { throw { code, message }; };
const PERMIT_MS = 24 * 3_600_000;
export class TalkSim {
  #attention = new Map<string, string>();
  #queues = new Map<string, QueueView>();
  #permits = new Map<string, { task: number; queue: number; expires: number }>();
  /** Items that continue a task which stopped, rather than start a new one. */
  #continuations = new Set<string>();
  constructor(private host: SimHost, private start: (task: Task, prompt: string) => void) {
    for (const task of host.tasks.filter((t) => t.state === "accepted")) {
      const queue = this.queue(task.project_id);
      if (!queue.items.some((i) => i.task_id === task.task_id)) queue.items.push({
        item_id: uuidv7(Date.now()), task_id: task.task_id, position: queue.items.length,
        depends_on: [], authorization: "ask_before_start", state: { state: "waiting" },
        added_by: task.created_by, added_at: task.created_at,
      });
      this.put(queue, false);
    }
    for (const queue of [host.queue, ...this.#queues.values()]) {
      for (const item of queue.items) {
        const task = host.tasks.find((t) => t.task_id === item.task_id);
        const message = host.journal.find(({ event }) => event.type === "timeline_appended" && event.task_id === item.task_id && event.item.item.kind === "user_message")?.event;
        const body = message?.type === "timeline_appended" && message.item.item.kind === "user_message" ? message.item.item : null;
        item.request = { prompt: body?.text ?? task?.title ?? "Review this task", bundle: body?.bundle ?? null };
        if (body && message?.type === "timeline_appended" && item.state.state !== "started") host.trackStartMessage(item.task_id, message.item.seq);
        if (!body) this.recordPrompt(item.task_id, item.request.prompt, item.request.bundle);
        if (item.authorization === "auto_start" && task) this.authorize(queue, item, task);
      }
      this.refresh(queue); this.put(queue, false);
    }
  }
  recordPrompt(taskId: string, text: string, bundle: AnnotationBundle | null) {
    const seq = this.host.seq + 1;
    this.host.trackStartMessage(taskId, seq);
    this.host.emit({ type: "timeline_appended", task_id: taskId, item: { seq, at: Date.now(), run_id: null, item: { kind: "user_message", text, bundle, delivery: "host_accepted" } } });
  }
  queue(projectId: string): QueueView {
    if (this.host.queue.project_id === projectId) return structuredClone(this.host.queue);
    return structuredClone(this.#queues.get(projectId) ?? { project_id: projectId, revision: 1, paused: true, pause_reason: "Next up starts paused. Resume it to let tasks start.", writer: null, items: [] });
  }
  put(queue: QueueView, emit = true) {
    if (queue.project_id === this.host.queue.project_id) this.host.queue = queue;
    else this.#queues.set(queue.project_id, queue);
    this.attention(queue);
    if (emit) this.host.emit({ type: "queue_changed", queue });
  }
  attention(queue: QueueView) {
    const front = queue.items.find(item => item.state.state !== "started");
    // As on the host: raised once the go-ahead alone holds the front back, so its age is the wait on the person.
    const task = !queue.paused && front?.state.state === "awaiting_start"
      ? this.host.tasks.find(t => t.task_id === front.task_id && t.state === "accepted" && ((t.mode ?? "work") !== "work" || !queue.writer)) : undefined;
    const previous = this.host.attention.find(a => a.attention_id === this.#attention.get(queue.project_id));
    if (previous && previous.resolved_at === null && previous.task_id === task?.task_id) return;
    const recount = (taskId: string) => {
      const current = this.host.tasks.find(t => t.task_id === taskId);
      if (current) { current.attention_count = blockingAttentionCount(this.host.attention, taskId); this.host.emit({ type: "task_upserted", task: current }); }
    };
    if (previous && previous.resolved_at === null) {
      previous.resolved_at = Date.now(); this.host.emit({ type: "attention_resolved", attention_id: previous.attention_id });
      if (previous.task_id) recount(previous.task_id);
    }
    this.#attention.delete(queue.project_id);
    if (task) {
      const item = { attention_id: uuidv7(Date.now()), task_id: task.task_id, run_id: null, approval_id: null,
        kind: "waiting_for_you" as const, title: task.title, body: "Next up needs your go-ahead · Review and start",
        created_at: Date.now(), resolved_at: null, priority: 70 };
      this.host.attention.push(item); this.#attention.set(queue.project_id, item.attention_id);
      this.host.emit({ type: "attention_upserted", item }); recount(task.task_id);
    }
  }
  refresh(queue: QueueView) {
    // As on the host: Run all keeps the go-ahead of each fix it waits to start.
    const running = this.host.github?.running(queue.project_id);
    for (const item of queue.items) {
      if (item.state.state === "started" || !running?.has(item.task_id)) continue;
      const task = this.host.tasks.find((t) => t.task_id === item.task_id);
      const p = this.#permits.get(item.item_id);
      if (!task || (item.authorization === "auto_start" && p && p.task === task.revision && p.queue === queue.revision
        && p.expires === item.authorization_expires_at && p.expires - Date.now() > PERMIT_MS / 2)) continue;
      item.authorization = "auto_start"; this.authorize(queue, item, task);
    }
    for (const item of queue.items) {
      if (item.state.state === "started") continue;
      if (item.authorization === "auto_start" && (!item.authorization_expires_at || item.authorization_expires_at <= Date.now())) {
        item.authorization = "ask_before_start"; item.authorization_expires_at = undefined;
      }
      let reason: string | null = null;
      for (const id of item.depends_on) {
        const parent = queue.items.find((i) => i.item_id === id);
        const run = parent?.state.state === "started" ? this.host.runs.find((r) => parent.state.state === "started" && r.run_id === parent.state.run_id) : undefined;
        if (!parent) reason = "A task this depends on was removed";
        else if (!run || !["completed", "failed", "interrupted", "reconciliation_required"].includes(run.state)) reason = "Waiting for an earlier task";
        else if (run.state !== "completed" || run.outcome?.unknowns.length || run.outcome?.still_active.length || run.children.some((c) => c.state === "active" || c.state === "unknown")) reason = "An earlier task did not finish successfully";
      }
      item.state = reason ? { state: "blocked", reason } : { state: item.authorization === "ask_before_start" ? "awaiting_start" : "waiting" };
    }
  }
  bump(queue: QueueView) {
    const before = queue.revision++;
    for (const item of queue.items) { const p = this.#permits.get(item.item_id); if (p?.queue === before) p.queue = queue.revision; }
  }
  invalidate(queue: QueueView) {
    queue.revision++;
    queue.items = queue.items.map((i, position) => i.state.state === "started" ? { ...i, position } : { ...i, position, authorization: "ask_before_start", authorization_expires_at: undefined });
  }
  /** As on the host: only `changed`, and what waits on them, ask again. */
  withdraw(queue: QueueView, changed: string[]) {
    const asks = waitingOn(queue.items, changed);
    for (const id of asks) this.#permits.delete(id);
    this.bump(queue);
    queue.items = queue.items.map((i, position) => i.state.state === "started" || !asks.has(i.item_id) ? { ...i, position } : { ...i, position, authorization: "ask_before_start", authorization_expires_at: undefined });
  }
  authorize(queue: QueueView, item: QueueItem, task: Task) {
    item.authorization_expires_at = Date.now() + PERMIT_MS;
    this.#permits.set(item.item_id, { task: task.revision, queue: queue.revision, expires: item.authorization_expires_at });
  }
  enqueue(task: Task, prompt: string, bundle: AnnotationBundle | null, placement: Extract<TaskPlacement, { mode: "queue" }>): string {
    const queue = this.queue(task.project_id);
    // As on the host: adding keeps what may start by itself, and one to start by itself resumes Next up.
    this.bump(queue);
    if (placement.authorization === "auto_start") { queue.paused = false; queue.pause_reason = undefined; }
    const item: QueueItem = { item_id: uuidv7(Date.now()), task_id: task.task_id, position: placement.position ?? queue.items.length,
      authorization: placement.authorization, state: { state: "waiting" }, depends_on: [], added_by: task.created_by, added_at: Date.now(), request: { prompt, bundle } };
    queue.items.splice(item.position, 0, item); queue.items.forEach((i, p) => i.position = p);
    if (item.authorization === "auto_start") this.authorize(queue, item, task);
    this.refresh(queue); this.put(queue);
    return item.item_id;
  }
  /** As on the host: a stopped task's next message waits its turn in Next up. */
  continueLater(task: Task, prompt: string, bundle: AnnotationBundle | null, authorization: QueueAuthorization) {
    const queue = this.queue(task.project_id);
    if (queue.items.some((i) => i.task_id === task.task_id && i.state.state !== "started"))
      fail("conflict", "This task is already waiting in Next up. Edit its message there.");
    this.#continuations.add(this.enqueue(task, prompt, bundle, { mode: "queue", position: null, authorization }));
  }
  started(task: Task, runId: string) {
    const queue = this.queue(task.project_id); this.bump(queue);
    const item = queue.items.find((i) => i.task_id === task.task_id && i.state.state !== "started");
    if (item) { item.state = { state: "started", run_id: runId }; item.authorization = "ask_before_start"; item.authorization_expires_at = undefined; }
    if ((task.mode ?? "work") === "work") queue.writer = runId;
    this.refresh(queue); this.put(queue);
  }
  released(projectId: string, runId: string) {
    const queue = this.queue(projectId);
    if (queue.writer === runId) { this.bump(queue); queue.writer = null; }
    this.refresh(queue); this.put(queue);
    // As on the host, only the agent's own completion starts what waits next;
    // a failure, a Stop or a person's reconciliation starts nothing more.
    const run = this.host.runs.find((r) => r.run_id === runId);
    if (run?.state === "completed" && !run.human_resolution) this.advance(projectId);
    const task = run && this.host.tasks.find((t) => t.task_id === run.task_id);
    if (task) void this.host.github.ended(task);
  }
  canStart(queue: QueueView, item: QueueItem, task: Task, explicitStart = false) {
    if (queue.paused && !explicitStart) fail("conflict", "Next up is paused. Resume it before starting.");
    if (queue.items.find((i) => i.state.state !== "started")?.item_id !== item.item_id) fail("conflict", "An earlier task is still waiting.");
    if (item.state.state === "blocked") fail("conflict", item.state.reason);
    if (task.state !== "accepted" && !this.#continuations.has(item.item_id)) fail("conflict", "This task is no longer waiting.");
    if ((task.mode ?? "work") === "work" && this.host.projects.find((p) => p.project_id === task.project_id)?.active_writer) fail("conflict", "Another task is changing files.");
  }
  advance(projectId: string) {
    const queue = this.queue(projectId); this.refresh(queue); this.put(queue);
    const item = queue.items.find((i) => i.state.state !== "started");
    if (!item || item.authorization !== "auto_start") return;
    const task = this.host.tasks.find((t) => t.task_id === item.task_id);
    const permit = this.#permits.get(item.item_id);
    if (!task || !permit || permit.queue !== queue.revision || permit.task !== task.revision || permit.expires <= Date.now() || task.kind === "computer_use") return;
    try { this.canStart(queue, item, task); } catch { return; }
    this.#permits.delete(item.item_id); this.start(task, item.request?.prompt ?? "");
  }
  handle(command: Command, expected: number | null): { result: CommandResult | null } | null {
    if (!command.command.startsWith("queue_") || command.command === "queue_continuation") return null;
    const projectId = "project_id" in command ? command.project_id : [this.host.queue, ...this.#queues.values()].find((q) => q.items.some((i) => "item_id" in command && i.item_id === command.item_id))?.project_id;
    if (!projectId) return fail("not_found", "This queue item no longer exists.");
    let queue = this.queue(projectId);
    if (expected !== queue.revision) return fail("stale_revision", "The queue changed. Review it again.");
    if (command.command === "queue_pause" || command.command === "queue_resume") {
      if (projectId === this.host.queue.project_id) {
        this.host.work.handle(command, expected); queue = this.queue(projectId);
        // Existing race fixture can advance work before the pause wins.
        for (const p of this.#permits.values()) if (p.queue === expected) p.queue = queue.revision;
      } else { this.bump(queue); queue.paused = command.command === "queue_pause"; queue.pause_reason = queue.paused ? "Next up is paused. Resume it to let tasks start." : undefined; }
      this.refresh(queue); this.put(queue);
      if (!queue.paused) this.advance(projectId);
      return { result: null };
    }
    if (!("item_id" in command)) return null;
    const item = queue.items.find((i) => i.item_id === command.item_id);
    if (!item) return fail("not_found", "This item has left Next up.");
    if (item.state.state === "started") return fail("conflict", "This task already started.");
    const task = this.host.tasks.find((t) => t.task_id === item.task_id);
    if (!task) return fail("not_found", "This task no longer exists.");
    switch (command.command) {
      case "queue_start":
        if (command.task_revision !== task.revision) return fail("stale_revision", "The task changed. Review it again.");
        this.refresh(queue); this.canStart(queue, item, task, true); this.start(task, item.request?.prompt ?? ""); return { result: null };
      case "queue_edit_prompt": {
        if (!command.prompt.trim()) return fail("invalid_request", "Write the request first.");
        item.request = { prompt: command.prompt, bundle: item.request?.bundle ?? null };
        this.host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: this.host.seq + 1, at: Date.now(), run_id: null, item: { kind: "note", text: "Queued prompt updated" } } });
        this.recordPrompt(task.task_id, command.prompt, item.request.bundle);
        const next = { ...task, revision: task.revision + 1, updated_at: Date.now(), mission: task.mission ? { ...task.mission, goal: command.prompt } : null };
        this.host.tasks = this.host.tasks.map((t) => t.task_id === task.task_id ? next : t); this.host.emit({ type: "task_upserted", task: next }); this.withdraw(queue, [item.item_id]); break;
      }
      case "queue_move": {
        const error = queueMoveError(queue, item.item_id, command.position); if (error) return fail("conflict", error);
        queue.items.splice(queue.items.indexOf(item), 1); queue.items.splice(command.position, 0, item); this.invalidate(queue); break;
      }
      case "queue_remove": {
        if (task.state !== "accepted" || task.current_run !== null) return fail("conflict", "This task already started.");
        const removed: Task = { ...task, state: "archived", revision: task.revision + 1, updated_at: Date.now(), status_line: "Removed from Next up before starting" };
        this.host.tasks = this.host.tasks.map((t) => t.task_id === task.task_id ? removed : t);
        this.host.emit({ type: "task_upserted", task: removed });
        this.#permits.delete(item.item_id);
        queue.items = queue.items.filter((i) => i.item_id !== item.item_id); this.withdraw(queue, [item.item_id]); break;
      }
      case "queue_set_dependencies": {
        const reaches = (id: string, seen = new Set<string>()): boolean => {
          if (id === item.item_id) return true;
          if (seen.has(id)) return false;
          seen.add(id);
          return queue.items.find((i) => i.item_id === id)?.depends_on.some((dep) => reaches(dep, seen)) ?? false;
        };
        if (command.depends_on.some((id) => reaches(id))) return fail("conflict", "These dependencies form a cycle.");
        if (command.depends_on.some((id) => !queue.items.some((i) => i.item_id === id && i.position < item.position))) return fail("conflict", "Choose tasks earlier in Next up.");
        item.depends_on = command.depends_on; this.withdraw(queue, [item.item_id]); break;
      }
      case "queue_set_authorization":
        this.bump(queue); item.authorization = command.authorization;
        if (item.authorization === "auto_start") this.authorize(queue, item, task); else { item.authorization_expires_at = undefined; this.#permits.delete(item.item_id); } break;
      default: return null;
    }
    this.refresh(queue); this.put(queue);
    if (command.command === "queue_set_authorization") this.advance(projectId);
    return { result: null };
  }
}
