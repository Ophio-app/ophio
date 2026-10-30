// SPDX-License-Identifier: Apache-2.0
import type { Trail, TrailEvent } from "../protocol/types";
import type { StorageLike } from "../state/savedJson";
import { MAX_TRAIL_EVENTS, PHONE_TRAIL_MS } from "./constants";
import { SavedDiagnostics } from "./storage";

export class PhoneTrail {
  #ring: (TrailEvent | undefined)[] = new Array(MAX_TRAIL_EVENTS);
  #head = 0;
  #size = 0;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #saved: SavedDiagnostics<TrailEvent[]>;
  constructor(readonly now: () => number = Date.now, storage?: StorageLike | null) {
    this.#saved = new SavedDiagnostics("diagnostics.trail.v1", storage);
    const events = this.#saved.load([]);
    if (Array.isArray(events)) for (const e of events.slice(-MAX_TRAIL_EVENTS)) {
      if (e && Number.isFinite(e.at) && e.event && typeof e.event.type === "string" && e.at >= now() - PHONE_TRAIL_MS && e.at <= now()) this.#append(e);
    }
  }
  #append(e: TrailEvent): void {
    if (this.#size === MAX_TRAIL_EVENTS) { this.#head = (this.#head + 1) % MAX_TRAIL_EVENTS; this.#size--; }
    this.#ring[(this.#head + this.#size++) % MAX_TRAIL_EVENTS] = e;
  }
  #prune(): void {
    const oldest = this.now() - PHONE_TRAIL_MS;
    while (this.#size && this.#ring[this.#head]!.at < oldest) {
      this.#ring[this.#head] = undefined;
      this.#head = (this.#head + 1) % MAX_TRAIL_EVENTS;
      this.#size--;
    }
  }
  record(event: Trail): void {
    try {
      this.#prune();
      this.#append({ at: this.now(), event });
      this.#timer ??= setTimeout(() => this.flush(), 1000);
    } catch { /* Diagnostics must never interrupt the action being recorded. */ }
  }
  snapshot(): TrailEvent[] {
    this.#prune();
    return Array.from({ length: this.#size }, (_, i) => structuredClone(this.#ring[(this.#head + i) % MAX_TRAIL_EVENTS]!));
  }
  flush = (): void => {
    try { clearTimeout(this.#timer); this.#timer = undefined; this.#saved.save(this.snapshot()); } catch { /* Best effort if storage is full. */ }
  };
  followLifecycle(): () => void {
    const changed = () => {
      this.record({ type: "lifecycle", state: document.visibilityState === "hidden" ? "background" : "foreground" });
      if (document.visibilityState === "hidden") this.flush();
    };
    window.addEventListener("pagehide", this.flush);
    document.addEventListener("visibilitychange", changed);
    return () => { window.removeEventListener("pagehide", this.flush); document.removeEventListener("visibilitychange", changed); this.flush(); };
  }
}
export const trail = new PhoneTrail();
