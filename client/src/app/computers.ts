// SPDX-License-Identifier: Apache-2.0
//
// Every paired computer's session while the app is open: one connection to
// each, so the header's menu shows how each is doing and All can combine
// what they hold. The native layer allows one control connection per
// computer, so this is the only place a computer's session is made.
//
// What a session needs while the person isn't looking at its computer runs
// here too: what it heard is kept for the next cold start, and its alerts
// are raised (or left to the background watcher). A computer the screen
// doesn't show lets go of its connection once the app has been out of sight
// for a while, and connects again as the app returns, so a phone in a
// pocket keeps no more connections open than it did with one computer.

import type { HostProfile, Platform } from "../host/link";
import { HostSession } from "../host/session";
import { cachedState, cacheObservations } from "../platform/offline";
import { watchAttention, type Viewed } from "../platform/notifications";
import { Store } from "../state/store";
import { takenName } from "./computerName";

/** How long the app may be out of sight before computers it doesn't show
 * let go of their connections. A quick look at another app keeps them. */
export const PARK_AFTER_MS = 30_000;

export interface ComputersOptions {
  /** Computers whose session must stay connected whatever is shown, such as
   * one still confirming that control ended. */
  held?: (profileId: string) => boolean;
  /** The computer named itself, as the owner renamed it there. */
  named?: (profileId: string, name: string) => void;
}

interface Kept {
  session: HostSession;
  profile: HostProfile;
  stop: () => void;
}

export class Computers extends Store<ReadonlyMap<string, HostSession>> {
  #kept = new Map<string, Kept>();
  #profiles: HostProfile[] = [];
  /** Computers let go of while the app is out of sight. */
  #parked = new Set<string>();
  #parkTimer: ReturnType<typeof setTimeout> | null = null;
  #holders = 0;
  #running = false;
  /** The computer the screen shows, whose alerts it shows. */
  readonly viewing = new Store<string | null>(null);
  /** Every visibly shown computer, independently of the active navigation side. */
  readonly visible = new Store<ReadonlySet<string>>(new Set());
  #visibleCounts = new Map<string, number>();
  visibleCount(profileId: string): number { return this.#visibleCounts.get(profileId) ?? 0; }
  show(profileId: string): () => void {
    this.session(profileId);
    this.#visibleCounts.set(profileId, (this.#visibleCounts.get(profileId) ?? 0) + 1);
    this.visible.set(new Set(this.#visibleCounts.keys()));
    let done = false;
    return () => {
      if (done) return;
      done = true;
      const count = (this.#visibleCounts.get(profileId) ?? 1) - 1;
      if (count) this.#visibleCounts.set(profileId, count); else this.#visibleCounts.delete(profileId);
      this.visible.set(new Set(this.#visibleCounts.keys()));
    };
  }
  /** All's own lists are on screen: every connected computer's requests
   * and results are on it, and their alerts with them. */
  readonly showingAll = new Store(false);
  #allShown = 0;

  /** All's lists are on screen until the returned function says they
   * aren't; another computer's requests alert as usual otherwise. */
  showAll(): () => void {
    this.#allShown++;
    this.showingAll.set(true);
    let done = false;
    return () => {
      if (done) return;
      done = true;
      if (--this.#allShown === 0) this.showingAll.set(false);
    };
  }
  /** Names asked of a computer and not yet announced by it, by profile. */
  #expected = new Map<string, string>();
  /** The last name each computer was followed to, so a name this device
   * couldn't save isn't asked for again and again. */
  #told = new Map<string, string>();

  /** Nothing connects until a session is asked for or the app holds this. */
  constructor(readonly platform: Platform, profiles: HostProfile[], readonly options: ComputersOptions = {}) {
    super(new Map());
    this.#profiles = profiles;
  }

  /** Connect to every paired computer until the returned function lets go.
   * Letting go and holding again at once, as React's development checks
   * do, keeps the connections. */
  hold(): () => void {
    this.#holders++;
    if (!this.#running) {
      this.#running = true;
      document.addEventListener("visibilitychange", this.#visibility);
    }
    this.sync(this.#profiles);
    let released = false;
    return () => {
      if (released) return;
      released = true;
      if (--this.#holders > 0) return;
      queueMicrotask(() => { if (this.#holders === 0) this.#close(); });
    };
  }

  /** Follow the paired computers: connect to new ones, let go of forgotten ones. */
  sync(profiles: HostProfile[]): void {
    this.#profiles = profiles;
    if (!this.#running) return;
    let changed = false;
    for (const [id, kept] of this.#kept) {
      const now = profiles.find((p) => p.profileId === id);
      if (!now || now.hostId !== kept.profile.hostId) { this.#drop(id); changed = true; }
      else if (now.name !== kept.profile.name) this.#rewire(id, now);
    }
    for (const id of this.#parked) if (!profiles.some((p) => p.profileId === id)) this.#parked.delete(id);
    for (const p of profiles) {
      if (this.#kept.has(p.profileId) || this.#parked.has(p.profileId)) continue;
      this.#open(p);
      changed = true;
    }
    if (changed) this.#publish();
  }

  /** The session for a paired computer, connecting it first if it was let go.
   * `known` is its profile, for one paired since the last `sync`. */
  session(profileId: string, known?: HostProfile): HostSession {
    const kept = this.#kept.get(profileId);
    if (kept) return kept.session;
    let profile = this.#profiles.find((p) => p.profileId === profileId);
    if (!profile && known?.profileId === profileId) this.#profiles = [...this.#profiles, (profile = known)];
    if (!profile) throw new Error("This computer is no longer paired with this device.");
    this.#parked.delete(profileId);
    const session = this.#open(profile);
    this.#publish();
    return session;
  }

  /** The session of a computer, if it is connected or connecting. */
  peek(profileId: string): HostSession | undefined {
    return this.#kept.get(profileId)?.session;
  }

  /** The screen now shows this computer. */
  view(profileId: string | null): void {
    if (profileId && !this.#kept.has(profileId) && this.#profiles.some((p) => p.profileId === profileId)) this.session(profileId);
    this.viewing.set(profileId);
  }

  /** This device asked the computer to take `name`: until the computer
   * announces it, an announcement of the name before is not followed, so
   * the new one shown at once stays. */
  expectName(profileId: string, name: string): void { this.#expected.set(profileId, name); }
  /** The computer refused the name, or it no longer matters. */
  settleName(profileId: string): void { this.#expected.delete(profileId); }

  /** Stop and forget a computer's session, as when it is unpaired here. */
  forget(profileId: string): void {
    this.#parked.delete(profileId);
    this.#expected.delete(profileId);
    this.#told.delete(profileId);
    if (!this.#kept.has(profileId)) return;
    this.#drop(profileId);
    this.#publish();
  }

  #close(): void {
    this.#running = false;
    document.removeEventListener("visibilitychange", this.#visibility);
    if (this.#parkTimer) clearTimeout(this.#parkTimer);
    this.#parkTimer = null;
    this.#parked.clear();
    for (const id of [...this.#kept.keys()]) this.#drop(id);
    this.#publish();
  }

  #open(profile: HostProfile): HostSession {
    const session = new HostSession(profile.profileId, this.platform.control(profile.profileId));
    const cached = cachedState(profile);
    if (cached) session.state.set(cached);
    const kept: Kept = { session, profile, stop: () => {} };
    this.#kept.set(profile.profileId, kept);
    this.#wire(kept);
    session.start();
    return session;
  }

  /** What runs for each computer while its session lives. */
  #wire(kept: Kept): void {
    const { session, profile } = kept;
    const id = profile.profileId;
    const viewed: Viewed = {
      get: () => this.showingAll.get() || this.visible.get().has(id) || this.viewing.get() === id,
      subscribe: (listener) => { const a = this.viewing.subscribe(listener), b = this.showingAll.subscribe(listener), c = this.visible.subscribe(listener); return () => { a(); b(); c(); }; },
    };
    // Returning to the app is when a person looks: try now, not after the wait
    // between attempts, and show what changed meanwhile in one step.
    const offForeground = this.platform.onForeground?.(() => {
      if (session.link.get().state === "offline") session.reconnect(); else session.foreground();
    });
    const offCache = cacheObservations(profile, session);
    const offAlerts = watchAttention(this.platform, session, profile.name, viewed);
    // A computer its owner renamed carries that name to every device paired
    // with it; one never renamed keeps the name this device gave it, unless
    // that name came from the computer, which then went back to its own.
    const named = () => {
      const host = session.state.get().host;
      if (!host || host.host_id !== profile.hostId) return;
      const expected = this.#expected.get(id);
      if (expected !== undefined) {
        if (host.name === expected) this.#expected.delete(id);
        return;
      }
      if (host.name === profile.name || host.name === this.#told.get(id)) return;
      if (!host.renamed && takenName(id) !== profile.name) return;
      this.#told.set(id, host.name);
      this.options.named?.(id, host.name);
    };
    const offNamed = session.state.subscribe(named);
    named();
    kept.stop = () => { offForeground?.(); offCache(); offAlerts(); offNamed(); };
  }

  /** The same session under a profile renamed on this device. */
  #rewire(id: string, profile: HostProfile): void {
    const kept = this.#kept.get(id);
    if (!kept) return;
    kept.stop();
    kept.profile = profile;
    this.#wire(kept);
  }

  #drop(id: string): void {
    const kept = this.#kept.get(id);
    if (!kept) return;
    this.#kept.delete(id);
    kept.stop();
    kept.session.stop();
  }

  #publish(): void {
    this.set(new Map([...this.#kept].map(([id, k]) => [id, k.session])));
  }

  #visibility = (): void => {
    if (document.visibilityState === "hidden") {
      this.#parkTimer ??= setTimeout(() => { this.#parkTimer = null; this.#park(); }, PARK_AFTER_MS);
      return;
    }
    if (this.#parkTimer) { clearTimeout(this.#parkTimer); this.#parkTimer = null; }
    this.#unpark();
  };

  #park(): void {
    if (document.visibilityState !== "hidden") return;
    const viewed = this.viewing.get();
    let changed = false;
    for (const id of [...this.#kept.keys()]) {
      if (id === viewed || this.visible.get().has(id) || this.options.held?.(id)) continue;
      this.#drop(id);
      this.#parked.add(id);
      changed = true;
    }
    if (changed) this.#publish();
  }

  #unpark(): void {
    if (!this.#parked.size) return;
    const parked = [...this.#parked];
    this.#parked.clear();
    for (const id of parked) {
      const profile = this.#profiles.find((p) => p.profileId === id);
      if (profile && !this.#kept.has(id)) this.#open(profile);
    }
    this.#publish();
  }
}
