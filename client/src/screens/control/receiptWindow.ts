// SPDX-License-Identifier: Apache-2.0
import type { HostSession } from "../../host/session";
import type { HostState, InterventionRecord } from "../../state/projection";
import { Store } from "../../state/store";

/** Observe from the shell so opening a sheet cannot revive an old receipt. */
class ReceiptWindow extends Store<ReadonlySet<number>> {
  readonly #seen = new Map<number, string>();
  readonly #expired = new Set<number>();
  #boot: string | null = null;
  constructor(session: HostSession) {
    super(new Set());
    session.state.subscribe(() => this.update(session.state.get()));
    this.update(session.state.get());
  }
  private update(state: HostState) {
    if (this.#boot !== state.bootEpoch) {
      for (const seq of this.#seen.keys()) this.#expired.add(seq);
      this.#seen.clear();
      this.#boot = state.bootEpoch;
    }
    const alive = new Set(state.interventions.map((r) => r.seq));
    for (const seq of this.#seen.keys()) if (!alive.has(seq)) {
      this.#seen.delete(seq);
      this.#expired.delete(seq);
    }
    for (const seq of this.#expired) if (!alive.has(seq)) this.#expired.delete(seq);
    const visible = new Set<number>();
    for (const record of state.interventions) {
      const current = signature(state, record);
      const before = this.#seen.get(record.seq);
      if (before === undefined) this.#seen.set(record.seq, current);
      else if (before !== current) this.#expired.add(record.seq);
      if (!this.#expired.has(record.seq)) visible.add(record.seq);
    }
    const previous = this.get();
    if (visible.size !== previous.size || [...visible].some((seq) => !previous.has(seq))) this.set(visible);
  }
}

function signature(state: HostState, record: InterventionRecord): string {
  const task = record.taskId ? state.tasks[record.taskId] : undefined;
  const target = record.targetId ?? task?.target_id;
  const views = target ? [state.control[target]] : record.receipt.kind === "lock_desktop" ? Object.values(state.control) : [];
  return JSON.stringify([
    task?.current_run, task?.state, task?.current_run ? state.runs[task.current_run]?.state : undefined,
    views.map((view) => view && [view.target_id, view.input_state, view.lease?.lease_id, view.lease?.state,
      view.lease?.generation, view.model_observation?.state, view.private_step?.requested_at, view.private_step?.ready]),
  ]);
}

const windows = new WeakMap<HostSession, ReceiptWindow>();
export function receiptWindow(session: HostSession): Store<ReadonlySet<number>> {
  let window = windows.get(session);
  if (!window) { window = new ReceiptWindow(session); windows.set(session, window); }
  return window;
}
