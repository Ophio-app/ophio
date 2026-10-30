// SPDX-License-Identifier: Apache-2.0
import { useCallback, useEffect, useState } from "react";
import { describeError, useApp, useLink } from "../../app/context";
import type { HostSession } from "../../host/session";
import type { VoiceView } from "../../protocol/types";
import { useRoute } from "../../shell/Screen";
const cache = new WeakMap<HostSession, Map<string, VoiceView>>();
export function useVoice(sessionId?: string, active = true) {
  const { session, profile } = useApp();
  const online = useLink().state === "online";
  const key = sessionId ?? "settings";
  // A view is shown only for the session it was asked about.
  const [loaded, setLoaded] = useState<{ key: string; view: VoiceView } | null>(null);
  const view = loaded?.key === key ? loaded.view : cache.get(session)?.get(key) ?? null;
  const [error, setError] = useState<string | null>(null);
  const [tick, refresh] = useState(0);
  const reload = useCallback(() => refresh(n => n + 1), []);
  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    if (!online || !active) return;
    const query = async () => {
      try {
        const { view } = await session.query({ kind: "voice", session_id: sessionId ?? null });
        if (disposed) return;
        let saved = cache.get(session); if (!saved) { saved = new Map(); cache.set(session, saved); }
        saved.set(key, view); setLoaded({ key, view }); setError(null);
      } catch (e) { if (!disposed) setError(describeError(e, profile.name)); }
      if (!disposed && sessionId) timer = setTimeout(() => void query(), 1000);
    };
    void query();
    return () => { disposed = true; clearTimeout(timer); };
  }, [session, profile.name, key, sessionId, active, online, tick]);
  return { view, error, reload };
}
export function useVoiceVisible() { return useRoute().active; }
