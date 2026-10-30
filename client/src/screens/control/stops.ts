// SPDX-License-Identifier: Apache-2.0
import { CommandError, NotConnectedError, OutcomeUnknownError, type HostSession } from "../../host/session";
import { uuidv7 } from "../../protocol/ids";
import type { CommandReceipt, InterventionReceipt, ProtocolError, Task } from "../../protocol/types";
import { isTaskRunning, refreshFromSnapshot, type HostState } from "../../state/projection";
import { Store } from "../../state/store";

export const STOP_UNKNOWN = "Stop not confirmed. It will be retried when the computer is connected; it may already have stopped.";
/** Admission is not proof of interruption. Never gate a working run forever. */
export const STOP_WAIT_MS = 20_000;
export const STOP_AGAIN = "Still reported running. Stop again asks the agent to interrupt again; it does not force quit.";
export interface StopIntent {
  taskId: string;
  runId: string | null;
  revision: number;
  id: string;
  at: number;
  acceptedAt?: number;
  count?: number;
  force?: boolean;
  afterSeq?: number;
  bootEpoch?: string | null;
  /** Refused as stale while this run was still the task's: resend once this
   * phone has seen the task at this revision and the run is still current. */
  awaitRevision?: number;
  phase: "still_running" | "waiting" | "sending" | "unknown" | "accepted" | "refused" | "not_running";
  detail: string;
}
function isStopIntent(value: unknown): value is StopIntent {
  if (!value || typeof value !== "object") return false;
  const record = value as Record<string, unknown>;
  return typeof record.taskId === "string" && typeof record.id === "string"
    && (record.runId === null || typeof record.runId === "string")
    && typeof record.revision === "number" && Number.isSafeInteger(record.revision) && record.revision >= 0
    && typeof record.at === "number" && Number.isFinite(record.at)
    && typeof record.detail === "string" && typeof record.phase === "string"
    && ["still_running", "waiting", "sending", "unknown", "accepted", "refused", "not_running"].includes(record.phase)
    && (record.acceptedAt === undefined || (typeof record.acceptedAt === "number" && Number.isFinite(record.acceptedAt)))
    && (record.afterSeq === undefined || (typeof record.afterSeq === "number" && Number.isSafeInteger(record.afterSeq)))
    && (record.bootEpoch === undefined || record.bootEpoch === null || typeof record.bootEpoch === "string")
    && (record.awaitRevision === undefined || (typeof record.awaitRevision === "number" && Number.isSafeInteger(record.awaitRevision)));
}
/** One persisted admission identity per Stop. Navigation never owns its lifetime. */
export class StopStore extends Store<Record<string, StopIntent>> {
  readonly #session: HostSession;
  readonly #key: string;
  #busy = new Set<string>();
  #off: (() => void)[] = [];
  #ready = false;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #restored = new Set<string>();
  constructor(session: HostSession) {
    const key = `control-stops.${session.profileId}`;
    let saved: Record<string, StopIntent> = {};
    try {
      const parsed: unknown = JSON.parse(localStorage.getItem(key) ?? "{}");
      if (parsed && typeof parsed === "object") {
        for (const value of Object.values(parsed)) {
          if (isStopIntent(value)) {
            const intent = value;
            saved[intent.taskId] = intent.phase === "sending" ? { ...intent, phase: "unknown", detail: STOP_UNKNOWN } : intent;
          }
        }
      }
    } catch { /* An unreadable record cannot authorize a replay. */ }
    super(saved);
    for (const intent of Object.values(saved)) if (intent.phase === "accepted") this.#restored.add(intent.id);
    this.#session = session;
    this.#key = key;
    this.#off = [session.link.subscribe(() => this.reconcile()), session.state.subscribe(() => this.reconcile()), session.commands.subscribe(() => this.receipts())];
    queueMicrotask(() => { if (this.#off.length) this.reconcile(); });
  }
  dispose() { for (const off of this.#off) off(); this.#off = []; clearTimeout(this.#timer); }
  update(intent: StopIntent) {
    this.set((s) => ({ ...s, [intent.taskId]: intent }));
    this.persist();
  }
  private persist() {
    try { localStorage.setItem(this.#key, JSON.stringify(this.get())); } catch { /* The inline state survives while this session is open. */ }
  }
  private remove(intent: StopIntent) {
    if (this.get()[intent.taskId]?.id !== intent.id) return;
    this.set((s) => { const next = { ...s }; delete next[intent.taskId]; return next; });
    this.#restored.delete(intent.id);
    this.persist();
  }
  request(task: Task, force = false) {
    const previous = this.get()[task.task_id];
    if (!force && previous && previous.runId === task.current_run && !["still_running", "refused", "not_running"].includes(previous.phase)) {
      if (["waiting", "unknown"].includes(previous.phase)) void this.send(previous);
      return;
    }
    const intent: StopIntent = { taskId: task.task_id, runId: task.current_run, revision: task.revision,
      id: uuidv7(Date.now()), at: Date.now(), force, count: (previous?.runId === task.current_run ? previous.count ?? 1 : 0) + (force ? 0 : 1), afterSeq: this.#session.state.get().journalHead, bootEpoch: this.#session.state.get().bootEpoch, phase: "waiting", detail: STOP_UNKNOWN };
    this.update(intent);
    void this.send(intent);
  }
  reconcile() {
    clearTimeout(this.#timer);
    const state = this.#session.state.get();
    const ready = this.#session.online && state.caughtUp;
    const connected = ready && !this.#ready;
    this.#ready = ready;
    let nextWait = Infinity;
    for (const intent of Object.values(this.get())) {
      const task = state.tasks[intent.taskId];
      // An empty cache before catch-up cannot prove that a run disappeared.
      if (ready && (!task || task.current_run !== intent.runId || !intent.runId || !state.runs[intent.runId] || !isTaskRunning(state, task))) {
        this.remove(intent);
        continue;
      }
      if (stopAcknowledged(state, intent)) {
        this.remove(intent);
        continue;
      }
      if (intent.phase === "accepted") {
        const remaining = (intent.acceptedAt ?? intent.at) + STOP_WAIT_MS - Date.now();
        const activity = state.activity[intent.taskId];
        const newer = (task?.updated_at !== undefined && task.updated_at > (intent.acceptedAt ?? intent.at) && state.runs[intent.runId ?? ""]?.state === "working")
          || (activity?.runId === intent.runId && activity.at > (intent.acceptedAt ?? intent.at))
          || Object.values(state.liveText).some((t) => t.runId === intent.runId && t.at > (intent.acceptedAt ?? intent.at));
        if (remaining <= 0 || newer || (ready && (this.#restored.has(intent.id) || (intent.bootEpoch !== undefined && intent.bootEpoch !== state.bootEpoch)))) {
          this.update({ ...intent, phase: "still_running", detail: STOP_AGAIN });
          this.#restored.delete(intent.id);
        } else nextWait = Math.min(nextWait, remaining);
      }
      if (["waiting", "unknown"].includes(intent.phase)) {
        if (intent.awaitRevision === undefined) { if (connected) void this.send(intent); }
        else if (ready && task && task.revision >= intent.awaitRevision) void this.send({ ...intent, revision: task.revision, awaitRevision: undefined });
      }
    }
    if (Number.isFinite(nextWait)) this.#timer = setTimeout(() => this.reconcile(), Math.max(1, nextWait));
  }
  private accepted(intent: StopIntent, receipt?: CommandReceipt | null) {
    const current = this.get()[intent.taskId];
    // An acknowledgement, ended run or newer request may have arrived first.
    if (current?.id !== intent.id || !["waiting", "sending", "unknown"].includes(current.phase)) return;
    if (receipt?.result?.kind === "intervention" && confirmsStop(receipt.result.receipt)) {
      this.remove(intent);
      return;
    }
    this.update({ ...current, acceptedAt: Date.now(), phase: "accepted", detail: current.force ? "Force stop sent. Waiting for the agent to end…" : "Stop sent. Waiting for the agent to stop…" });
    this.reconcile();
  }
  async send(intent: StopIntent) {
    const session = this.#session;
    if (this.#busy.has(intent.id) || !session.online || !session.state.get().grants?.tasks) return;
    const task = session.state.get().tasks[intent.taskId];
    if (!task || task.current_run !== intent.runId || !isTaskRunning(session.state.get(), task)) return;
    this.#busy.add(intent.id);
    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      this.update({ ...intent, phase: "sending", detail: "Stop requested. Waiting for the computer…" });
      // The same id is safe to resubmit even after a lost receipt; host admission
      // deduplicates it. The original revision never targets a later run.
      const unanswered = new Promise<never>((_, reject) => { timeout = setTimeout(() => reject(new OutcomeUnknownError(intent.id)), 8_000); });
      const receipt = await Promise.race([unanswered, session.command({ command: "stop_task", task_id: intent.taskId, ...(intent.force ? { force: true } : {}) }, {
        commandId: intent.id, expectedRevision: intent.revision, label: `Stop · ${task.title}`,
      })]);
      this.accepted(intent, receipt);
    } catch (error) {
      if (this.get()[intent.taskId]?.id !== intent.id) return;
      if (error instanceof CommandError && error.error.code !== "outcome_unknown") {
        this.refused(intent, error.error);
      } else {
        this.update({ ...intent, phase: error instanceof NotConnectedError ? "waiting" : "unknown", detail: STOP_UNKNOWN });
        session.commands.set((commands) => {
          const command = commands[intent.id];
          return command?.phase === "sending" ? { ...commands, [intent.id]: { ...command, phase: "unknown" } } : commands;
        });
      }
    } finally {
      if (timeout) clearTimeout(timeout);
      this.#busy.delete(intent.id);
    }
  }
  private refused(intent: StopIntent, error: ProtocolError) {
    if (this.get()[intent.taskId]?.id !== intent.id || this.get()[intent.taskId]?.phase === "not_running") return;
    // Every step a working agent takes moves its task's revision. While the run
    // is unchanged, Stop it again, under a new id, at the revision the computer
    // named once this phone has seen it; reconcile drops the Stop first if a
    // later run began.
    if (error.code === "stale_revision" && this.#session.state.get().tasks[intent.taskId]?.current_run === intent.runId) {
      const current = typeof error.detail === "number" && Number.isSafeInteger(error.detail) ? error.detail : intent.revision + 1;
      this.update({ ...intent, id: uuidv7(Date.now()), phase: "waiting", detail: "The agent took another step before Stop arrived. Sending Stop again for this run…", awaitRevision: current });
      this.reconcile();
      return;
    }
    const ended = ["already_resolved", "conflict"].includes(error.code)
      && /not running|already ended|no active run|finished/i.test(error.message);
    this.update({ ...intent, phase: ended ? "not_running" : "refused", detail: ended
      ? "The computer reports this task is no longer running. Its result is being checked." : error.message });
    if (ended) void this.#session.query({ kind: "snapshot" }).then((snapshot) => {
      this.#session.state.set((s) => refreshFromSnapshot(s, snapshot, Date.now()));
    }).catch(() => undefined);
  }
  private receipts() {
    for (const intent of Object.values(this.get())) {
      if (!["sending", "unknown"].includes(intent.phase)) continue;
      const command = this.#session.commands.get()[intent.id];
      if (command?.phase === "accepted" || command?.phase === "duplicate")
        this.accepted(intent, command.receipt);
      else if (command?.phase === "rejected" && command.error && command.error.code !== "outcome_unknown")
        this.refused(intent, command.error);
    }
  }
}
/** Receipts have named facts, not a typed acknowledgement flag. Match only
 * explicit agent outcomes, never desktop withdrawal or an admission receipt.
 * Computers before 2026-10-15, and receipts they kept, say "acknowledged". */
function confirmsStop(receipt: InterventionReceipt): boolean {
  return receipt.kind === "stop_task" && receipt.facts.some((f) => f.status === "done"
    && /^(agent (confirmed|acknowledged) stop|agent exited|agent process ended on the computer)$/i.test(f.label));
}
function stopAcknowledged(state: HostState, intent: StopIntent): boolean {
  if (intent.bootEpoch !== undefined && intent.bootEpoch !== state.bootEpoch) return false;
  return state.interventions.some((r) => r.taskId === intent.taskId
    && (intent.afterSeq !== undefined ? r.seq > intent.afterSeq : r.receipt.requested_at >= intent.at) && confirmsStop(r.receipt))
    || (state.timelines[intent.taskId]?.items.some((item) => item.run_id === intent.runId
      && (intent.afterSeq !== undefined ? item.seq > intent.afterSeq : item.at >= intent.at)
      && item.item.kind === "receipt" && confirmsStop(item.item.receipt)) ?? false);
}
const stores = new WeakMap<HostSession, StopStore>();
export function stopStore(session: HostSession): StopStore {
  let store = stores.get(session);
  if (!store) { store = new StopStore(session); stores.set(session, store); }
  return store;
}

export function stopFrequency(count: number): string {
  return count === 1 ? "once" : count === 2 ? "twice" : `${count} times`;
}
