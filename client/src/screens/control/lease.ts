// SPDX-License-Identifier: Apache-2.0
import type {
  InputAction,
  InputEnvelope,
  LeaseView,
} from "../../protocol/types";

export const FRAME_STALE_MS = 3_000;
export function frameFresh(at: number | null, now: number): boolean {
  return at !== null && now - at <= FRAME_STALE_MS;
}
export function renewalDelay(
  windowMs: number,
  visible: boolean,
  fresh: boolean,
): number | null {
  return visible && fresh && Number.isFinite(windowMs) && windowMs > 0
    ? Math.max(50, Math.floor(windowMs / 3))
    : null;
}

/** One sequencer per host session; remounting cannot reuse a sequence number. */
export class InputSender {
  #sequences = new Map<string, number>();
  #retired = new Set<string>();
  #held = new Map<string, InputAction[]>();
  constructor(private readonly transmit: (env: InputEnvelope) => boolean) {}
  send(lease: LeaseView, revision: number, action: InputAction): boolean {
    if (this.#retired.has(lease.lease_id)) return false;
    const seq = (this.#sequences.get(lease.lease_id) ?? 0) + 1;
    this.#sequences.set(lease.lease_id, seq);
    const ok = this.transmit({
      lease_id: lease.lease_id,
      generation: lease.generation,
      geometry_revision: revision,
      seq,
      action,
    });
    if (ok) {
      const held = this.#held.get(lease.lease_id) ?? [];
      if (action.action === "key_press") {
        const next = held.filter(
          (a) => a.action !== "key_press" || a.key !== action.key,
        );
        if (action.pressed) next.push({ ...action, pressed: false });
        this.#held.set(lease.lease_id, next);
      } else if (action.action === "pointer_button") {
        const next = held.filter(
          (a) => a.action !== "pointer_button" || a.button !== action.button,
        );
        if (action.pressed) next.push({ ...action, pressed: false });
        this.#held.set(lease.lease_id, next);
      }
    }
    return ok;
  }
  retire(lease: LeaseView, revision: number): void {
    this.release(lease, revision);
    this.#retired.add(lease.lease_id);
  }
  retired(lease: LeaseView): boolean {
    return this.#retired.has(lease.lease_id);
  }
  release(lease: LeaseView, revision: number): void {
    for (const action of this.#held.get(lease.lease_id) ?? [])
      this.send(lease, revision, action);
    this.#held.delete(lease.lease_id);
    this.send(lease, revision, { action: "release_all" });
  }
}

/** Stop scheduling immediately when the observed safety condition is lost. */
export function scheduleRenewal(
  windowMs: number,
  eligible: () => boolean,
  renew: () => void,
): () => void {
  const delay = renewalDelay(windowMs, true, true);
  if (delay === null) return () => undefined;
  let stopped = false;
  let timer: ReturnType<typeof setTimeout>;
  const tick = () => {
    if (stopped || !eligible()) return;
    renew();
    timer = setTimeout(tick, delay);
  };
  timer = setTimeout(tick, delay);
  return () => {
    stopped = true;
    clearTimeout(timer);
  };
}
