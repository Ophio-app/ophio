// SPDX-License-Identifier: Apache-2.0
import { useEffect } from "react";
import { reportReference as reference } from "../diagnostics/reference";
import { useApp } from "../app/context";
import { REPORTS_FEATURE } from "../diagnostics/constants";
import { reports } from "../diagnostics/reports";
import type { HostSession } from "../host/session";
import type { ReportSummary, ReportsView } from "../protocol/types";
import { Store, useStore } from "../state/store";

export class ComputerReports extends Store<{ view: ReportsView | null; error: unknown }> {
  #asked = 0;
  #followers = 0;
  #stop: (() => void) | null = null;
  constructor(readonly session: HostSession) { super({ view: null, error: null }); }
  supported() { return !!this.session.state.get().capabilities?.features.includes(REPORTS_FEATURE); }
  async refresh() {
    const asked = ++this.#asked;
    if (!this.supported()) { this.set({ view: null, error: null }); return; }
    if (this.session.link.get().state !== "online") return;
    try {
      const { view } = await this.session.query({ kind: "reports" });
      if (asked === this.#asked && this.supported()) this.set({ view, error: null });
    } catch (error) { if (asked === this.#asked) this.set(s => ({ ...s, error })); }
  }
  follow() {
    if (this.#followers++ > 0) return () => this.release();
    let key = "";
    const check = () => {
      const next = `${this.supported()}/${this.session.link.get().state}/${this.session.state.get().reportsRevision}`;
      if (next !== key) { key = next; void this.refresh(); }
    };
    const state = this.session.state.subscribe(check), link = this.session.link.subscribe(check);
    check();
    this.#stop = () => { state(); link(); };
    return () => this.release();
  }
  release() { if (--this.#followers === 0) { this.#stop?.(); this.#stop = null; } }
}
const stores = new WeakMap<HostSession, ComputerReports>();
export function computerReports(session: HostSession) {
  let store = stores.get(session);
  if (!store) { store = new ComputerReports(session); stores.set(session, store); }
  return store;
}
export function useReports() {
  const { session } = useApp();
  const remote = computerReports(session);
  const state = useStore(remote, s => s);
  const local = useStore(reports, s => s);
  useEffect(() => remote.follow(), [remote]);
  const queued: ReportSummary[] = local.queued.map(({ bundle: b }) => ({ report_id: b.report_id, reference: reference(b.report_id), created_at: b.created_at, origin: b.origin, signature: b.signature, words: b.words, history: [{ at: b.created_at, status: { state: "queued" } }], questions: [] }));
  const all = new Map([...queued, ...local.direct, ...(remote.supported() ? state.view?.reports ?? [] : [])].map(r => [r.report_id, r]));
  return { ...state, view: remote.supported() ? state.view : null, reports: [...all.values()].sort((a, b) => b.created_at - a.created_at), remote, local };
}
