// SPDX-License-Identifier: Apache-2.0
import { useCallback, useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import type { ReleaseCheckView } from "../../protocol/types";

export const RELEASE_CHECK_FEATURE = "release_check_v1";

export function useReleaseCheck() {
  const { session, profile } = useApp();
  const link = useLink();
  const supported = useHost((s) => s.capabilities?.features.includes(RELEASE_CHECK_FEATURE) ?? false);
  const live = useHost((s) => s.host?.release_check ?? null);
  const [view, setView] = useState<ReleaseCheckView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [revision, refresh] = useState(0);
  const reload = useCallback(() => refresh((n) => n + 1), []);
  useEffect(() => {
    let cancelled = false;
    setView(null);
    if (live) setError(null);
    if (!supported || link.state !== "online" || live) return;
    void session.query({ kind: "release_check" }).then(({ view }) => {
      if (cancelled) return;
      setView(view); setError(null);
    }).catch((e: unknown) => { if (!cancelled) setError(describeError(e, profile.name)); });
    return () => { cancelled = true; };
  }, [session, profile.name, link.state, supported, live, revision]);
  return { view: live ?? view, error, supported, reload };
}
