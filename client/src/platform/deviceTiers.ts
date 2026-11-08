// SPDX-License-Identifier: Apache-2.0
//
// This device's choice of how each type of notification shows: Alert,
// Silent or Off. It belongs to the device, not the computer: one choice
// covers every paired computer, and a tablet can be quieter than the phone.
//
// On Android the choice lives in Android's own notification categories, one
// per type, which the watcher reads before each post and Android's settings
// can change too; the app reads it back each time it returns. Where there is
// no watcher (iPhone, a browser) it is kept in the app's storage and applies
// to the notifications the app shows while it is open.

import type { AlertsState, Platform } from "../host/link";
import type { AlertType, Tier } from "../protocol/types";
import { Store } from "../state/store";
import { backgroundAlerts, type Alerts } from "./alerts";
import { ALERT_TYPES, isAlertType, isTier, recommendedTiers } from "./alertTypes";
import { reportPlatformError } from "./errors";

export type Tiers = Record<AlertType, Tier>;

export interface DeviceTiersView {
  tiers: Tiers;
  /** Android keeps the choice and the watcher applies it, closed or open. */
  native: boolean;
  /** Read at least once from where it is kept. */
  ready: boolean;
}

const KEY = "alert-tiers";

/** Every type, each with the recommended choice where `saved` names none. */
function complete(saved: Partial<Record<string, unknown>> | undefined): Tiers {
  const tiers = recommendedTiers();
  for (const t of ALERT_TYPES) {
    const v = saved?.[t.type];
    if (isTier(v)) tiers[t.type] = v;
  }
  return tiers;
}

function readLocal(): Tiers {
  try {
    const raw = localStorage.getItem(KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : undefined;
    return complete(parsed && typeof parsed === "object" ? (parsed as Record<string, unknown>) : undefined);
  } catch {
    return complete(undefined);
  }
}

export class DeviceTiers extends Store<DeviceTiersView> {
  #watcher: Alerts | null;
  #stop: (() => void) | null = null;
  #users = 0;

  constructor(readonly platform: Platform) {
    const watcher = backgroundAlerts(platform);
    const native = !!watcher && !!platform.alerts?.setTier;
    super({ tiers: native ? complete(watcher?.get().state?.tiers) : readLocal(), native, ready: !native });
    this.#watcher = native ? watcher : null;
  }

  /** Follows Android's answer while anything shows or uses the choices. */
  start(): () => void {
    const watcher = this.#watcher;
    if (!watcher) return () => {};
    if (this.#users++ === 0) {
      const read = (state: AlertsState | null) => {
        if (!state) return;
        // An app build whose watcher predates types keeps its choices here.
        if (!state.tiers) { this.set((s) => ({ ...s, ready: true })); return; }
        const tiers = complete(state.tiers);
        this.set((s) => ({ ...s, tiers, ready: true }));
      };
      const off = watcher.subscribe(() => read(watcher.get().state));
      const stopWatcher = watcher.start();
      read(watcher.get().state);
      this.#stop = () => { off(); stopWatcher(); };
    }
    return () => { if (--this.#users === 0) { this.#stop?.(); this.#stop = null; } };
  }

  tier(type: AlertType): Tier {
    return this.get().tiers[type];
  }

  /** Sets one type's choice. Resolves once it is in force; rejects with
   * the reason, in the person's words, when it isn't. */
  async setTier(type: AlertType, tier: Tier): Promise<void> {
    if (!isAlertType(type)) throw new Error("This kind of notification isn’t known to this app.");
    const watcher = this.#watcher;
    if (watcher) {
      const state = await watcher.setTier(type, tier);
      if (state?.tiers) this.set((s) => ({ ...s, tiers: complete(state.tiers), ready: true }));
      else this.set((s) => ({ ...s, tiers: { ...s.tiers, [type]: tier } }));
      return;
    }
    const tiers = { ...this.get().tiers, [type]: tier };
    try {
      localStorage.setItem(KEY, JSON.stringify(tiers));
    } catch {
      reportPlatformError({ operation: "Notifications", message: "Your notification choice couldn’t be saved. Free some device storage and try again." });
      throw new Error("It couldn’t be saved on this device.");
    }
    this.set((s) => ({ ...s, tiers }));
  }
}

const all = new WeakMap<Platform, DeviceTiers>();
export function deviceTiers(platform: Platform): DeviceTiers {
  let tiers = all.get(platform);
  if (!tiers) { tiers = new DeviceTiers(platform); all.set(platform, tiers); }
  return tiers;
}
