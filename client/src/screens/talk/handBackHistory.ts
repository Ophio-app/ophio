// SPDX-License-Identifier: Apache-2.0
import type { HostSession } from "../../host/session";
import { Store } from "../../state/store";
import type { Row } from "./timeline";

export interface HandBackRecord {
  id: string; taskId: string; runId: string; at: number; requestedAt: number; history: string; summary: string;
}
const stores = new WeakMap<HostSession, Store<HandBackRecord[]>>();
/** A small local rendering of observed command receipts for hosts without a
 * hand-back timeline entry. Admission alone never adds a conversation line. */
export function handBackHistory(session: HostSession): Store<HandBackRecord[]> {
  let store = stores.get(session);
  if (!store) {
    let saved: HandBackRecord[] = [];
    try {
      const data: unknown = JSON.parse(localStorage.getItem(`hand-backs.${session.profileId}`) ?? "[]");
      if (Array.isArray(data)) saved = data.filter((r): r is HandBackRecord => r &&
        [r.id, r.taskId, r.runId, r.history, r.summary].every(v => typeof v === "string") &&
        Number.isFinite(r.at) && Number.isFinite(r.requestedAt));
    } catch { /* Local history is optional. */ }
    store = new Store(saved);
    const current = store;
    store.subscribe(() => {
      try { localStorage.setItem(`hand-backs.${session.profileId}`, JSON.stringify(current.get())); } catch { /* Still visible this session. */ }
    });
    stores.set(session, store);
  }
  return store;
}
export function withHandBacks(rows: Row[], records: HandBackRecord[]): Row[] {
  const extra: Row[] = records.filter(r => !rows.some(row =>
    (row.kind === "receipt" || row.kind === "note") && row.at >= r.requestedAt && row.at <= r.at + 1000 &&
    /handed back|returned? to (the )?agent/i.test(row.kind === "receipt" ? row.summary : row.text),
  )).map(r => ({ kind: "receipt", key: `hand-back-${r.id}`, at: r.at, summary: r.summary, outcome: "confirmed" }));
  const result = [...rows];
  for (const row of extra) {
    const next = result.findIndex(r => r.at > row.at);
    result.splice(next < 0 ? result.length : next, 0, row);
  }
  return result;
}
