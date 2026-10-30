// SPDX-License-Identifier: Apache-2.0
import { useEffect, useMemo, useState } from "react";

export function countdown(ms: number): string {
  const seconds = Math.max(0, Math.ceil(ms / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}
/** Anchor the host's expiry when received; an old shared age ticker must not extend it. */
export function expiryRemaining(expiresAt: number, wallAtMount: number, elapsed: number): number {
  return Math.max(0, expiresAt - wallAtMount - Math.max(0, elapsed));
}
export function useExpiry(expiresAt: number): number {
  const anchor = useMemo(() => ({ wall: Date.now(), monotonic: performance.now() }), [expiresAt]);
  const [elapsed, setElapsed] = useState(0);
  useEffect(() => {
    const tick = () => setElapsed(performance.now() - anchor.monotonic);
    tick(); const timer = setInterval(tick, 250);
    return () => clearInterval(timer);
  }, [anchor]);
  return expiryRemaining(expiresAt, anchor.wall, elapsed);
}
