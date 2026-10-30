// SPDX-License-Identifier: Apache-2.0
//
// What this device has asked for, shown before the computer has done it. A
// tap shows its result at once; the computer's word follows, and where it
// refuses, what was shown goes back.

import type { HostStateStore } from "../app/screenNames";
import type { HostState } from "../state/projection";

/** How long a change, once the computer has done it, waits for the
 * computer's own state to show it before it is taken off anyway. */
export const SETTLE_MS = 5_000;

/**
 * Takes a change off once the computer's own state shows it (`landed`), so
 * nothing flickers back between the command's answer and its events, or
 * after `SETTLE_MS` at the latest.
 */
export function settle(state: HostStateStore, drop: () => void, landed: (s: HostState) => boolean): void {
  if (landed(state.base())) return drop();
  const done = () => { clearTimeout(timer); off(); drop(); };
  const timer = setTimeout(done, SETTLE_MS);
  const off = state.subscribe(() => { if (landed(state.base())) done(); });
}
