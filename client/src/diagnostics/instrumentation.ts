// SPDX-License-Identifier: Apache-2.0
import type { UiStore } from "../app/ui";
import type { HostSession, PendingCommand } from "../host/session";
import type { LinkState } from "../host/link";
import type { CommandOutcome, ErrorCode, NoticeTone } from "../protocol/types";
import { routePattern, screenOn } from "./routes";
import { trail } from "./trail";

export function recordNotice(key: string, tone: NoticeTone): void { trail.record({ type: "notice", key, tone }); }
export function followRoutes(ui: UiStore): void {
  let previous = "";
  const changed = () => {
    const route = routePattern(screenOn(ui.get()));
    if (route !== previous) { previous = route; trail.record({ type: "route", route }); }
  };
  changed(); ui.subscribe(changed);
}
export function recordConnection(link: LinkState): void {
  const state = link.state === "online" ? link.route ?? "direct"
    : link.state === "connecting" || link.state === "authenticating" ? "reconnecting"
    : link.state === "failed" && link.reason === "not_paired" ? "unpaired" : "offline";
  trail.record({ type: "connection", state });
}
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
  session.link.subscribe(() => recordConnection(session.link.get()));
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
