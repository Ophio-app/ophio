// SPDX-License-Identifier: Apache-2.0
import { useApp } from "../../app/context";
import type { HostSession } from "../../host/session";
import { Store, useStore } from "../../state/store";

export interface TerminalEnd {
  createdAt: number;
  /** This device's first observation, not an inferred process exit time. */
  observedAt: number;
  /** A completed Close requested by this device, never mere admission. */
  closedAt?: number;
}

export class TerminalHistory extends Store<Record<string, TerminalEnd>> {
  readonly key: string;
  constructor(private readonly session: HostSession) {
    const key = `terminal.ends.${session.profileId}`;
    const saved: Record<string, TerminalEnd> = {};
    try {
      const value: unknown = JSON.parse(localStorage.getItem(key) ?? "{}");
      if (value && typeof value === "object") for (const [id, entry] of Object.entries(value)) {
        if (entry && typeof entry === "object" && Number.isFinite(entry.createdAt) && Number.isFinite(entry.observedAt) &&
          (entry.closedAt === undefined || Number.isFinite(entry.closedAt))) saved[id] = entry;
      }
    } catch { /* An unreadable device history cannot establish a user close. */ }
    super(saved);
    this.key = key;
    session.state.subscribe(() => this.reconcile());
    session.commands.subscribe(() => this.reconcile());
    this.reconcile();
  }
  private record(id: string, value: TerminalEnd) {
    this.set(s => ({ ...s, [id]: value }));
    try { localStorage.setItem(this.key, JSON.stringify(this.get())); } catch { /* Keep the observation for this session. */ }
  }
  closed(id: string, at: number) {
    const terminal = this.session.state.get().terminals.find(t => t.terminal_id === id);
    if (!terminal) return;
    const prior = this.get()[id];
    if (prior?.createdAt === terminal.created_at && prior.closedAt !== undefined) return;
    this.record(id, { createdAt: terminal.created_at, observedAt: prior?.observedAt ?? at, closedAt: at });
  }
  private reconcile() {
    const state = this.session.state.get();
    for (const terminal of state.terminals) {
      if (terminal.state.state !== "exited") continue;
      const previous = this.get()[terminal.terminal_id];
      if (!previous || previous.createdAt !== terminal.created_at)
        this.record(terminal.terminal_id, { createdAt: terminal.created_at, observedAt: Date.now() });
    }
    for (const command of Object.values(this.session.commands.get())) {
      const stage = state.commandStages[command.id];
      if (command.body.command === "close_terminal" && stage?.stage === "observed" && !stage.error)
        this.closed(command.body.terminal_id, stage.at);
    }
  }
}

const histories = new WeakMap<HostSession, TerminalHistory>();
export function terminalHistory(session: HostSession): TerminalHistory {
  let history = histories.get(session);
  if (!history) { history = new TerminalHistory(session); histories.set(session, history); }
  return history;
}
export function useTerminalEnds(): Record<string, TerminalEnd> {
  return useStore(terminalHistory(useApp().session), s => s);
}
