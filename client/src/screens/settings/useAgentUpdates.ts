// SPDX-License-Identifier: Apache-2.0
import { useCallback, useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import type { HostSession } from "../../host/session";
import type { AgentUpdatesView } from "../../protocol/types";

/** The computer's word that it can keep its own copies of the agents up to date. */
export const AGENT_UPDATES_FEATURE = "agent_updates_v1";
/** How soon a check under way is looked at again. */
export const CHECKING_POLL_MS = 3_000;
const cache = new WeakMap<HostSession, AgentUpdatesView>();

export function useAgentUpdates() {
  const { session, profile } = useApp();
  const link = useLink();
  const supported = useHost((s) => s.capabilities?.features.includes(AGENT_UPDATES_FEATURE) ?? false);
  // New copies change the agents' versions and models.
  const harnesses = useHost((s) => s.harnesses);
  const [view, setView] = useState<AgentUpdatesView | null>(cache.get(session) ?? null);
  const [error, setError] = useState<string | null>(null);
  const [revision, refresh] = useState(0);
  // Counts the looks that have ended, answered or not.
  const [looked, setLooked] = useState(0);
  const reload = useCallback(() => refresh((n) => n + 1), []);
  useEffect(() => {
    let cancelled = false;
    setView(cache.get(session) ?? null);
    if (!supported || link.state !== "online") return;
    void session.query({ kind: "agent_updates" }).then(({ view }) => {
      if (cancelled) return;
      cache.set(session, view); setView(view); setError(null);
    }).catch((e: unknown) => { if (!cancelled) setError(describeError(e, profile.name)); })
      .finally(() => { if (!cancelled) setLooked((n) => n + 1); });
    return () => { cancelled = true; };
  }, [session, profile.name, link.state, supported, harnesses, revision]);
  // A check under way ends with the versions it found; look again after each
  // look, even one that failed, until it does.
  useEffect(() => {
    if (!view?.checking || link.state !== "online") return;
    const timer = setTimeout(reload, CHECKING_POLL_MS);
    return () => clearTimeout(timer);
  }, [view, looked, link.state, reload]);
  return { view, error, supported, reload };
}
