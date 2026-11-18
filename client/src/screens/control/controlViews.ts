// SPDX-License-Identifier: Apache-2.0
import type { HostSession } from "../../host/session";
import { refreshFromSnapshot } from "../../state/projection";
import { scheduleRenewal } from "./lease";

const snapshots = new WeakMap<HostSession, Promise<void>>();
/** Two display views arriving together need only one computer snapshot. */
export function reconcileControl(session: HostSession): Promise<void> {
  const current = snapshots.get(session);
  if (current) return current;
  const pending = session.query({ kind: "snapshot" }).then(snapshot => {
    session.state.set(state => refreshFromSnapshot(state, snapshot, Date.now()));
  }).finally(() => { if (snapshots.get(session) === pending) snapshots.delete(session); });
  snapshots.set(session, pending);
  return pending;
}
interface Candidate { eligible(): boolean; renew(): void }
interface Renewal { views: Set<Candidate>; stop(): void; running: boolean }
const renewals = new WeakMap<object, Map<string, Renewal>>();
/** Views can supervise the same lease, but only one renewal timer may send. */
export function shareRenewal(owner: object, key: string, windowMs: number, eligible: () => boolean, renew: () => void): () => void {
  let entries = renewals.get(owner);
  if (!entries) { entries = new Map(); renewals.set(owner, entries); }
  let entry = entries.get(key);
  if (!entry) { entry = { views: new Set(), stop() {}, running: false }; entries.set(key, entry); }
  const group = entry, candidate: Candidate = { eligible, renew };
  group.views.add(candidate);
  if (!group.running) {
    group.running = true;
    group.stop = scheduleRenewal(windowMs, () => {
      const allowed = [...group.views].some(view => view.eligible());
      if (!allowed) group.running = false;
      return allowed;
    }, () => [...group.views].find(view => view.eligible())?.renew());
  }
  return () => {
    group.views.delete(candidate);
    if (!group.views.size) { group.stop(); entries.delete(key); }
  };
}
const targets = new WeakMap<object, Map<string, number>>();
export function visibleControlViews(owner: object, target: string): number { return targets.get(owner)?.get(target) ?? 0; }
export function showControlTarget(owner: object, target: string): () => void {
  let counts = targets.get(owner);
  if (!counts) { counts = new Map(); targets.set(owner, counts); }
  counts.set(target, (counts.get(target) ?? 0) + 1);
  let removed = false;
  return () => {
    if (removed) return;
    removed = true;
    const count = (counts.get(target) ?? 1) - 1;
    if (count) counts.set(target, count); else counts.delete(target);
  };
}
interface MonitorView { stale(): boolean; pause(): void }
interface Monitor { views: Set<MonitorView>; timer: ReturnType<typeof setInterval> }
const monitors = new WeakMap<object, Map<string, Monitor>>();
/** A display's freshness checks share one timer, even when both sides watch it. */
export function monitorControlTarget(owner: object, target: string, stale: () => boolean, pause: () => void): () => void {
  let entries = monitors.get(owner);
  if (!entries) { entries = new Map(); monitors.set(owner, entries); }
  let entry = entries.get(target);
  if (!entry) {
    const views = new Set<MonitorView>();
    entry = { views, timer: setInterval(() => [...views].find(view => view.stale())?.pause(), 500) };
    entries.set(target, entry);
  }
  const group = entry, candidate = { stale, pause };
  group.views.add(candidate);
  return () => {
    group.views.delete(candidate);
    if (!group.views.size) { clearInterval(group.timer); entries.delete(target); }
  };
}
