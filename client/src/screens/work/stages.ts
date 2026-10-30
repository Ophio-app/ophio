// SPDX-License-Identifier: Apache-2.0
import type { CommandReceipt, ProjectStage, ProjectStageHistory, ProjectStageTransition, ProtocolError } from "../../protocol/types";
import { readLocal, writeLocal } from "./local";
import { Store } from "../../state/store";

export const stageName = (stage: ProjectStage | null | undefined): string => stage ? stage[0]?.toUpperCase() + stage.slice(1) : "Stage not set";
export const stageRevision = (current: ProjectStageTransition | null | undefined): number => current?.revision ?? 0;
export function staleStage(error: ProtocolError): string {
  const detail = error.detail;
  if (typeof detail === "object" && detail !== null && "current_stage" in detail) {
    const current = detail.current_stage;
    if (current === null) return "The stage is now not set. Your selection and note are kept. Review again before confirming.";
    if (typeof current === "string") return `The current stage is ${current}. Your selection and note are kept. Review again before confirming.`;
  }
  return "The stage changed on the computer. Your selection and note are kept. Refresh the history and review again.";
}

export interface StageAttempt {
  id: string;
  phase: "sending" | "unknown" | "recorded" | "refused" | "not_sent";
  message: string;
}
interface StageDraft {
  target: string;
  note: string;
  attempt: StageAttempt | null;
  saved: boolean;
}
function loadDraft(key: string): StageDraft {
  const value = readLocal<unknown>(key, null);
  const initial: StageDraft = { target: "", note: "", attempt: null, saved: true };
  if (!value || typeof value !== "object") return initial;
  const v = value as Record<string, unknown>;
  const a = v.attempt;
  let attempt: StageAttempt | null = null;
  if (a && typeof a === "object" && "id" in a && typeof a.id === "string" && "phase" in a && "message" in a && typeof a.message === "string") {
    const phase = a.phase;
    if (phase === "sending" || phase === "unknown" || phase === "recorded" || phase === "refused" || phase === "not_sent") {
      attempt = { id: a.id, phase: phase === "sending" ? "unknown" : phase, message: phase === "sending" ? "The app closed before the computer confirmed this change." : a.message };
    }
  }
  return { target: typeof v.target === "string" ? v.target : "", note: typeof v.note === "string" ? v.note : "", attempt, saved: true };
}
export class StageDraftStore extends Store<StageDraft> {
  constructor(private readonly key: string) { super(loadDraft(key)); }
  update(patch: Partial<StageDraft>): boolean {
    const next = { ...this.get(), ...patch };
    next.saved = writeLocal(this.key, next);
    this.set(next);
    return next.saved;
  }
  receipt(receipt: CommandReceipt): void {
    const attempt = this.get().attempt;
    if (attempt?.id !== receipt.command_id || attempt.phase === "recorded" || attempt.phase === "refused") return;
    const failure = receipt.error;
    if (receipt.status === "rejected" || failure) {
      this.update({ attempt: { id: receipt.command_id, phase: "refused", message: failure?.code === "stale_revision" ? staleStage(failure) : failure?.message ?? "The computer refused the stage change." } });
      return;
    }
    const result = receipt.result;
    if (result?.kind !== "project_stage_set") {
      if (this.get().attempt?.phase === "unknown") return;
      this.update({ attempt: { id: receipt.command_id, phase: "unknown", message: "Sent, but the computer hasn’t confirmed the new stage. Check it before changing it again." } });
      return;
    }
    this.update({ target: "", note: "", attempt: { id: receipt.command_id, phase: "recorded", message: `${stageName(result.transition.to)} recorded by ${result.transition.actor_name}.` } });
  }
}

export interface StageCache { history: ProjectStageHistory; name: string; observedAt: number }
/** Cached records are observations only; commands always quote a reviewed revision. */
export function readStageCache(key: string): StageCache | null {
  const raw = readLocal<unknown>(key, null);
  if (!record(raw) || typeof raw.name !== "string" || !timestamp(raw.observedAt)) return null;
  const h = raw.history;
  if (!record(h) || typeof h.project_id !== "string" || typeof h.has_more !== "boolean"
    || !Array.isArray(h.stages) || !h.stages.every(s => typeof s === "string")
    || typeof h.note_limit !== "number" || !Number.isSafeInteger(h.note_limit) || h.note_limit <= 0
    || !Array.isArray(h.transitions) || !h.transitions.every(transition)
    || (h.current !== null && !transition(h.current))) return null;
  return raw as unknown as StageCache;
}
function record(v: unknown): v is Record<string, unknown> { return typeof v === "object" && v !== null; }
function timestamp(v: unknown): v is number { return typeof v === "number" && Number.isFinite(v) && Math.abs(v) <= 8.64e15; }
function transition(v: unknown): boolean {
  if (!record(v) || !Number.isSafeInteger(v.revision) || typeof v.revision !== "number" || v.revision < 1
    || !timestamp(v.at) || typeof v.to !== "string" || (v.from !== null && typeof v.from !== "string")
    || (v.note !== null && typeof v.note !== "string") || typeof v.actor_name !== "string" || !record(v.principal)) return false;
  const p = v.principal;
  return p.kind === "local_owner" || p.kind === "system"
    || (p.kind === "device" && typeof p.device_id === "string")
    || (p.kind === "channel" && typeof p.identity_id === "string");
}
