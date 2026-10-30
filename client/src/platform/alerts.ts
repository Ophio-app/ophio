// SPDX-License-Identifier: Apache-2.0
import type { AlertChannel, AlertSettings, AlertsState, BackgroundAlerts, Platform } from "../host/link";
import { Store } from "../state/store";

interface AlertsView {
  state: AlertsState | null;
  pending: boolean;
  error: string | null;
}

/** The background watcher as Android and the watcher report it, shared by
 * Settings, the first-run screen and the app's own alerts that defer to it. */
export class Alerts extends Store<AlertsView> {
  #read = 0;
  #users = 0;
  #stop: (() => void) | null = null;

  constructor(readonly platform: Platform, readonly native: BackgroundAlerts) {
    super({ state: null, pending: false, error: null });
  }

  /** Reads again on each return to the app, as from Android's settings, and
   * as the app leaves the screen, when the app's own alerts defer to it. */
  start(): () => void {
    if (this.#users++ === 0) {
      const changed = () => { void this.refresh(); };
      document.addEventListener("visibilitychange", changed);
      const off = this.platform.onForeground?.(changed);
      void this.refresh();
      this.#stop = () => { document.removeEventListener("visibilitychange", changed); off?.(); };
    }
    return () => { if (--this.#users === 0) { this.#stop?.(); this.#stop = null; } };
  }

  /** Also reads every two seconds while a screen shows each computer's
   * connection: Android starts the watcher a moment after it is turned on. */
  follow(): () => void {
    const stop = this.start();
    const timer = setInterval(() => { if (document.visibilityState === "visible") void this.refresh(); }, 2000);
    return () => { clearInterval(timer); stop(); };
  }

  async refresh(): Promise<AlertsState | null> {
    const read = ++this.#read;
    try {
      const state = await this.native.state();
      if (read === this.#read) this.set((s) => ({ ...s, state, error: null }));
      return state;
    } catch {
      if (read === this.#read) this.set((s) => ({ ...s, error: "This device couldn’t check alerts for when the app is closed." }));
      return null;
    }
  }

  async configure(settings: AlertSettings): Promise<boolean> {
    const read = ++this.#read;
    this.set((s) => ({ ...s, pending: true, error: null }));
    try {
      const state = await this.native.configure(settings);
      if (read === this.#read) this.set((s) => ({ ...s, state }));
      return true;
    } catch (e) {
      // The choice can be saved even when Android refuses to start the watcher.
      await this.refresh();
      this.set((s) => ({ ...s, error: e instanceof Error ? e.message : "Your alert choice couldn’t be saved." }));
      return false;
    } finally {
      this.set((s) => ({ ...s, pending: false }));
    }
  }

  /** Android's battery dialog. The answer is read when the app returns. */
  battery(): Promise<boolean> {
    return this.#open(() => this.native.battery(), "Android’s battery settings couldn’t be opened.");
  }

  channel(channel: AlertChannel): Promise<boolean> {
    return this.#open(() => this.native.channel(channel), "Android’s notification settings couldn’t be opened.");
  }

  async #open(action: () => Promise<void>, failure: string): Promise<boolean> {
    this.set((s) => ({ ...s, error: null }));
    try {
      await action();
      return true;
    } catch (e) {
      this.set((s) => ({ ...s, error: e instanceof Error ? e.message : failure }));
      return false;
    }
  }

  /** Tells the watcher the app showed these alerts, so it does not raise them
   * once the app leaves the screen. */
  seen(profileId: string, attentionIds: string[]): void {
    if (attentionIds.length) void this.native.seen?.(profileId, attentionIds).catch(() => {});
  }

  /** Tells the watcher the app's screen shows this computer's alerts, `on`,
   * or no longer does. */
  viewing(profileId: string, on: boolean): void {
    void this.native.viewing?.(profileId, on).catch(() => {});
  }

  /** Whether the watcher raises this computer's alerts itself. It uses the
   * same tag, so an alert both raise shows once. */
  covers(profileId: string): boolean {
    const state = this.get().state;
    return !!state?.settings?.enabled && state.android.running && state.statuses[profileId] === "watching";
  }
}

const all = new WeakMap<Platform, Alerts>();
/** Null where the platform has no background watcher. */
const PRESET = "alerts.preset";
/** The answer about alerts given on the computer while pairing, which the
 * app's own question starts at. Kept until that question is answered. */
export const alertsPreset = {
  set(on: boolean) { localStorage.setItem(PRESET, on ? "on" : "off"); },
  get(): boolean | null { const v = localStorage.getItem(PRESET); return v === "on" ? true : v === "off" ? false : null; },
  clear() { localStorage.removeItem(PRESET); },
};

export function backgroundAlerts(platform: Platform): Alerts | null {
  if (!platform.alerts) return null;
  let alerts = all.get(platform);
  if (!alerts) { alerts = new Alerts(platform, platform.alerts); all.set(platform, alerts); }
  return alerts;
}
