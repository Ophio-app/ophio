// SPDX-License-Identifier: Apache-2.0
import type { UiStore } from "../app/ui";
import type { HostSession, PendingCommand } from "../host/session";
import type { LinkState } from "../host/link";
import type { CommandOutcome, ErrorCode, NoticeTone } from "../protocol/types";
import { routePattern, screenOn } from "./routes";
import { scrub } from "./scrub";
import { trail } from "./trail";

/** A notice shown again within this long of the last time is the same one
 * coming back, as on every try of a failing connection; the trail keeps one. */
export const NOTICE_REPEAT_MS = 10_000;
const noticed = new Map<string, number>();
/** `same` tells apart notices that share a key, such as toasts without a
 * code, for that check only; it never reaches the trail. */
export function recordNotice(key: string, tone: NoticeTone, same = "", now = Date.now()): void {
  const id = `${tone}:${key}:${same}`, last = noticed.get(id);
  if (last !== undefined && now - last < NOTICE_REPEAT_MS) return;
  noticed.set(id, now);
  if (noticed.size > 200) noticed.delete(noticed.keys().next().value!);
  trail.record({ type: "notice", key, tone });
}
export function followRoutes(ui: UiStore): void {
  let previous = "";
  const changed = () => {
    const route = routePattern(screenOn(ui.get()));
    if (route !== previous) { previous = route; trail.record({ type: "route", route }); }
  };
  changed(); ui.subscribe(changed);
}
/** Follows a link into the trail: each change of state, not each update of
 * one, and why it went offline, which the state alone doesn't say. */
export function connectionRecorder(): (link: LinkState) => void {
  let previous: string | null = null;
  const why = new Map<string, number>();
  return (link) => {
    const state = link.state === "online" ? link.route ?? "direct"
      : link.state === "connecting" || link.state === "authenticating" ? "reconnecting"
      : link.state === "failed" && link.reason === "not_paired" ? "unpaired" : "offline";
    if (state !== previous) trail.record({ type: "connection", state });
    previous = state;
    const [code, reason] = link.state === "offline" ? [`link_offline${link.code ? `:${link.code}` : ""}`, link.reason]
      : link.state === "failed" ? [`link_failed:${link.reason}`, link.detail] : [null, null];
    if (code === null) return;
    // A state that came without its reason still says it went offline.
    const message = typeof reason === "string" ? scrub(reason) : undefined, key = `${code}|${message ?? ""}`, now = Date.now(), last = why.get(key);
    if (last !== undefined && now - last < NOTICE_REPEAT_MS) return;
    why.set(key, now);
    trail.record({ type: "error", error: { code, ...(message ? { message } : {}) } });
  };
}
export function recordConnection(link: LinkState): void { connectionRecorder()(link); }
export function recordCommand(command: PendingCommand, outcome: CommandOutcome, now = Date.now(), code = command.error?.code): void {
  trail.record({ type: "command", command: command.body.command, outcome, ...(code ? { code } : {}), ms: Math.min(0xffffffff, Math.max(0, Math.round(now - command.issuedAt))) });
}
/** The stores include reconciled receipts and failures after admission too. */
export function instrumentSession(session: HostSession, now: () => number): void {
  const phases = new Map<string, string>();
  const stages = new Map<string, string>();
  // A command's acceptance and its observed effect are one outcome, not two.
  const outcomes = new Map<string, CommandOutcome>();
  const note = (c: PendingCommand, outcome: CommandOutcome, code?: ErrorCode) => {
    if (outcomes.get(c.id) === outcome) return;
    outcomes.set(c.id, outcome);
    recordCommand(c, outcome, now(), code);
  };
  const alerts = new Set<string>();
  let lastStages = session.state.get().commandStages;
  let lastAttention = session.state.get().attention;
  const connection = connectionRecorder();
  session.link.subscribe(() => connection(session.link.get()));
  session.commands.subscribe(() => {
    for (const c of Object.values(session.commands.get())) {
      if (phases.get(c.id) === c.phase) continue;
      phases.set(c.id, c.phase);
      const outcome: CommandOutcome = c.phase === "sending" ? "pending" : c.phase === "accepted" || c.phase === "duplicate" ? "accepted" : c.phase === "rejected" ? "refused" : c.phase === "unknown" ? "timed_out" : "failed";
      note(c, outcome);
    }
    for (const id of phases.keys()) if (!session.commands.get()[id]) { phases.delete(id); outcomes.delete(id); }
  });
  session.state.subscribe(() => {
    const state = session.state.get();
    if (state.commandStages !== lastStages) for (const [id, stage] of Object.entries(state.commandStages)) {
      if (stages.get(id) === stage.stage) continue;
      stages.set(id, stage.stage);
      const command = session.commands.get()[id];
      if (command && (stage.stage === "failed" || stage.stage === "observed")) note(command, stage.stage === "failed" ? "failed" : "accepted", stage.error?.code);
    }
    if (state.attention !== lastAttention) for (const item of Object.values(state.attention)) if (!alerts.has(item.attention_id)) {
      alerts.add(item.attention_id);
      if (item.resolved_at === null) trail.record({ type: "alert_received", ...(item.topic ? { topic: item.topic } : {}), quiet: item.quiet ?? false });
    }
    lastStages = state.commandStages;
    lastAttention = state.attention;
  });
}
