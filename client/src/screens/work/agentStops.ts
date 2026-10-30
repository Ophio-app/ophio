// SPDX-License-Identifier: Apache-2.0
import { CommandError, type HostSession } from "../../host/session";
import { uuidv7 } from "../../protocol/ids";
import type { Command, CommandResult, CommandStage, InterventionReceipt, ProtocolError } from "../../protocol/types";
import { Store } from "../../state/store";

type StopCommand = Extract<Command, { command: "stop_child" | "stop_observed_session" }>;
export interface AgentStopIntent {
  id: string;
  phase: "sending" | "accepted" | "unknown" | "refused" | "settled";
  detail: string;
  receipt?: InterventionReceipt;
}
export function stopProgress(intent: AgentStopIntent, stage?: CommandStage, result?: CommandResult | null, error?: ProtocolError | null): AgentStopIntent {
  if (result?.kind === "intervention" && intent.receipt === result.receipt) return intent;
  if (result?.kind === "intervention") return { ...intent, phase: "settled", detail: result.receipt.summary, receipt: result.receipt };
  if (error || stage === "uncertain" || stage === "reconciliation_required") return { ...intent,
    phase: error && error.code !== "outcome_unknown" ? "refused" : "unknown",
    detail: error?.message ?? "The computer hasn’t confirmed this Stop. Check its result before trying again." };
  if (stage === "failed") return { ...intent, phase: "refused", detail: "The computer could not send this Stop." };
  if (stage === "observed") return { ...intent, phase: "unknown", detail: "The computer finished handling Stop without a detailed result. Check the helper’s latest state." };
  return intent;
}

/** The lifetime of a Stop belongs to the session, so leaving a row or sheet
 * never drops an accepted request. Unknown requests are queried, never replayed. */
export class AgentStops extends Store<Record<string, AgentStopIntent>> {
  constructor(private readonly session: HostSession) {
    const restored: Record<string, AgentStopIntent> = {};
    try {
      const raw: unknown = JSON.parse(localStorage.getItem(`agent-stops.${session.profileId}`) ?? "{}");
      if (raw && typeof raw === "object") for (const [key, value] of Object.entries(raw).slice(-256)) {
        if (!value || typeof value !== "object") continue;
        const row = value as Record<string, unknown>;
        if (!key.includes(":") || key.length > 512 || typeof row.id !== "string" || !/^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i.test(row.id) || typeof row.detail !== "string" || typeof row.phase !== "string") continue;
        if (!["sending", "accepted", "unknown", "refused", "settled"].includes(row.phase)) continue;
        const phase = row.phase === "refused" ? "refused" : "unknown";
        restored[key] = { id: row.id, phase, detail: phase === "refused" ? row.detail : "Checking the Stop requested before the app closed…" };
      }
    } catch { /* A damaged record never authorizes a replay. */ }
    super(restored);
  }
  update(key: string, intent: AgentStopIntent) {
    this.set(s => ({ ...s, [key]: intent }));
    try { localStorage.setItem(`agent-stops.${this.session.profileId}`, JSON.stringify(Object.fromEntries(Object.entries(this.get()).slice(-256).map(([key, value]) => [key, { id: value.id, phase: value.phase, detail: value.detail }])))); }
    catch { /* The session still owns the request; no automatic resend is made. */ }
  }
  disconnected(key: string) {
    const intent = this.get()[key];
    if (intent && ["sending", "accepted"].includes(intent.phase)) this.update(key, { ...intent, phase: "unknown", detail: "The computer hasn’t confirmed this Stop. Check its result when connected." });
  }
  async request(key: string, command: StopCommand, revision: number, label: string) {
    const previous = this.get()[key];
    if (previous && ["sending", "accepted", "unknown"].includes(previous.phase)) return;
    await this.#send(key, command, revision, label, true);
  }
  async #send(key: string, command: StopCommand, revision: number, label: string, retryStale: boolean): Promise<void> {
    const intent: AgentStopIntent = { id: uuidv7(Date.now()), phase: "sending", detail: "Stop requested. Waiting for the computer…" };
    this.update(key, intent);
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const receipt = await Promise.race([
        this.session.command(command, { commandId: intent.id, expectedRevision: revision, label }),
        new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error("Stop outcome unknown")), 8000); }),
      ]);
      const current = this.get()[key] ?? intent;
      const accepted: AgentStopIntent = current.receipt ? current : { ...current, phase: "accepted", detail: "Stop sent. Waiting for the agent’s result…" };
      this.update(key, stopProgress(accepted, undefined, receipt.result, receipt.error));
      await this.check(key);
    } catch (e) {
      if (this.get()[key]?.receipt) return;
      // A working agent moves its task's revision with every step. The helper is
      // named exactly, so its Stop is sent once more at the task's current revision.
      if (retryStale && command.command === "stop_child" && e instanceof CommandError && e.error.code === "stale_revision" && this.get()[key]?.id === intent.id) {
        const state = this.session.state.get();
        const current = typeof e.error.detail === "number" ? e.error.detail : state.tasks[state.runs[command.run_id]?.task_id ?? ""]?.revision;
        if (current !== undefined && current !== revision) return this.#send(key, command, current, label, false);
      }
      this.update(key, stopProgress({ ...intent, phase: "unknown", detail: "The computer hasn’t confirmed this Stop. Check its result when connected." }, undefined, undefined, e instanceof CommandError ? e.error : undefined));
    } finally { if (timer) clearTimeout(timer); }
  }
  async check(key: string) {
    const intent = this.get()[key];
    if (!intent || !this.session.online) return;
    try {
      const status = await this.session.query({ kind: "command_status", command_id: intent.id });
      if (this.get()[key]?.id !== intent.id) return;
      this.update(key, stopProgress(this.get()[key] ?? intent, status.stage, status.result ?? status.receipt?.result, status.error ?? status.receipt?.error));
    } catch { /* Existing unknown/accepted state remains visible for the next check. */ }
  }
  reconcile() {
    for (const [key, intent] of Object.entries(this.get())) {
      const progress = this.session.state.get().commandStages[intent.id];
      if (progress) {
        const next = stopProgress(intent, progress.stage, progress.result, progress.error);
        if (next !== intent) this.update(key, next);
      }
    }
  }
}
const stores = new WeakMap<HostSession, AgentStops>();
export function agentStops(session: HostSession) {
  let store = stores.get(session);
  if (!store) { store = new AgentStops(session); stores.set(session, store); }
  return store;
}
