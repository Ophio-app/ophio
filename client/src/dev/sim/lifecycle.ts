// SPDX-License-Identifier: Apache-2.0
import type { AppLockState, HostProfile, Platform } from "../../host/link";
import { unavailableLock } from "../../platform/lock";

/** Opt-in native prompt fake, isolated from production authentication. */
export function simulatedLifecycle(enabledForTest: boolean) {
  let state: AppLockState = enabledForTest
    ? { available: true, reason: null, enabled: false, locked: false, relockMinutes: 0 } : unavailableLock;
  const listeners = new Set<(s: AppLockState) => void>();
  const links = new Set<(url: string) => void>();
  let complete: ((ok: boolean) => void) | null = null;
  const publish = () => { for (const receive of listeners) receive({ ...state }); return { ...state }; };
  const authenticate = async () => {
    if (!state.available) throw new Error(state.reason ?? "Lock unavailable");
    if (complete) throw new Error("An unlock request is already open.");
    if (!await new Promise<boolean>((resolve) => { complete = resolve; })) throw new Error("Not unlocked. Try again with your screen lock.");
  };
  const capabilities: Pick<Platform, "appLock" | "onAppLink"> = {
    appLock: {
      async state() { return { ...state }; },
      async configure(enabled, relockMinutes) { await authenticate(); state = { ...state, enabled, relockMinutes, locked: false }; return publish(); },
      async unlock() { await authenticate(); state = { ...state, locked: false }; return publish(); },
      // Without the lock test the simulator stands in for the phone's unlock,
      // so account screens can be exercised; production has no such path.
      async confirm() { if (enabledForTest) await authenticate(); return { ...state }; },
      async lock() { if (state.enabled) state = { ...state, locked: true }; return publish(); },
      listen(receive) { listeners.add(receive); return () => { listeners.delete(receive); }; },
    },
    onAppLink(receive) { links.add(receive); return () => { links.delete(receive); }; },
  };
  return { capabilities, controls: {
    get promptOpen() { return complete !== null; },
    authenticate(ok = true) { const resolve = complete; complete = null; resolve?.(ok); },
    lock() { if (enabledForTest) { state = { ...state, enabled: true, locked: true }; publish(); } },
    openLink(url: string) { for (const receive of links) receive(url); },
  } };
}
export function secondProfile(first: HostProfile): HostProfile {
  return { ...first, profileId: "sim-spare", hostId: "01900000-0000-7000-8000-000000000099", name: "spare", addresses: ["spare.tail1234.ts.net:47100"] };
}
