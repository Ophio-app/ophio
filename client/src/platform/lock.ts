// SPDX-License-Identifier: Apache-2.0
import type { AppLockState, Platform } from "../host/link";
import { Store } from "../state/store";

export const unavailableLock: AppLockState = {
  available: false, reason: "App lock requires Android’s secure screen lock. It isn’t available in this browser build.",
  enabled: false, locked: false, relockMinutes: 0,
};
export type LockView = AppLockState & { checking: boolean; busy: boolean; error: string | null };

/** Owns only local authentication. It cannot issue or replay host actions. */
export class LockController extends Store<LockView> {
  #off: (() => void) | undefined;
  constructor(readonly platform: Platform) {
    super({ ...unavailableLock, locked: !!platform.appLock, checking: !!platform.appLock, busy: false, error: null });
  }
  start(): () => void {
    const lock = this.platform.appLock;
    if (!lock) return () => {};
    this.#off = lock.listen((state) => this.accept(state));
    void this.run(() => lock.state());
    return () => { this.#off?.(); };
  }
  accept(state: AppLockState): void {
    this.set({ ...state, checking: false, busy: false, error: null });
  }
  async run(action: () => Promise<AppLockState>): Promise<void> {
    this.set((s) => ({ ...s, busy: true, error: null }));
    try { this.accept(await action()); }
    catch (e) {
      this.set((s) => ({ ...s, busy: false, checking: false,
        error: e instanceof Error ? e.message : "This device couldn’t unlock the app. Try again." }));
    }
  }
  unlock = (): Promise<void> => this.run(() => {
    if (!this.platform.appLock) return Promise.resolve(unavailableLock);
    return this.get().locked ? this.platform.appLock.unlock() : this.platform.appLock.state();
  });
  configure(enabled: boolean, minutes: AppLockState["relockMinutes"]): Promise<void> {
    return this.run(() => this.platform.appLock?.configure(enabled, minutes) ?? Promise.resolve(unavailableLock));
  }
  lock = (): Promise<void> => this.run(() => this.platform.appLock?.lock() ?? Promise.resolve(unavailableLock));
}
const controllers = new WeakMap<Platform, LockController>();
export function appLock(platform: Platform): LockController {
  let controller = controllers.get(platform);
  if (!controller) { controller = new LockController(platform); controllers.set(platform, controller); }
  return controller;
}
