// SPDX-License-Identifier: Apache-2.0
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useCommandControl } from "../../app/commandControl";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { Notice } from "../../design/Notice";
import type { HostSession } from "../../host/session";
import type { AgentPreferences as Preferences } from "../../protocol/types";

/** The computer's word that agents can be told not to ask questions. */
export const AGENT_PREFERENCES_FEATURE = "agent_preferences_v1";
const cache = new WeakMap<HostSession, Preferences>();

function usePreferences() {
  const { session, profile } = useApp();
  const link = useLink();
  const supported = useHost((s) => s.capabilities?.features.includes(AGENT_PREFERENCES_FEATURE) ?? false);
  const [view, setView] = useState<Preferences | null>(cache.get(session) ?? null);
  const [error, setError] = useState<string | null>(null);
  const [revision, refresh] = useState(0);
  const reload = useCallback(() => refresh((n) => n + 1), []);
  useEffect(() => {
    let cancelled = false;
    setView(cache.get(session) ?? null);
    if (!supported || link.state !== "online") return;
    void session.query({ kind: "agent_preferences" }).then(({ view }) => {
      if (cancelled) return;
      cache.set(session, view); setView(view); setError(null);
    }).catch((e: unknown) => { if (!cancelled) setError(describeError(e, profile.name)); });
    return () => { cancelled = true; };
  }, [session, profile.name, link.state, supported, revision]);
  return { view, error, supported, reload };
}

/** Whether agents stop to ask, or make their best choice and say so. */
export function AgentPreferences() {
  const { session, profile } = useApp();
  const link = useLink();
  const admin = useHost((s) => s.grants?.admin ?? false);
  const { view, error, supported, reload } = usePreferences();
  // The revision the last change saved, until the view shows it.
  const saved = useRef(0);
  const questions = useCommandControl({
    kind: "instant",
    value: view?.questions ?? true,
    describe: (e) => describeError(e, profile.name),
    send: async (next, note) => {
      const revision = Math.max(view?.revision ?? 0, saved.current);
      try {
        await session.command({ command: "set_agent_preferences", questions: next }, { label: note.label, expectedRevision: revision });
        saved.current = revision + 1;
      } finally { reload(); }
    },
  });
  const hint = useId();
  if (!supported) return null;
  const online = link.state === "online";
  const change = (next: boolean) => questions.set(next, {
    label: next ? "Let agents ask questions" : "Ask agents not to ask questions",
    what: "whether agents ask you questions",
    said: next ? "Agents ask when something is unclear." : "Agents make their best choice and say what they chose.",
  });
  return <article className="settings-card settings-stack">
    <label className="settings-check"><input type="checkbox" checked={questions.value} aria-describedby={hint}
      disabled={!view || !admin || !online}
      onChange={(e) => change(e.target.checked)} />Agents can ask me questions</label>
    <p id={hint} className="settings-meta">{!questions.value
      ? "Agents make the best choice they can and say what they chose and why, so you can change it. They still stop to ask when a choice can’t be undone or only you can know the answer."
      : "Agents ask when something is unclear, on your phone and in a notification, and wait for your answer."}</p>
    {!admin && <p className="settings-meta">This device needs the Manage access permission to change this.</p>}
    {!online && <Notice tone="unknown">Offline. Connect to change this.</Notice>}
    {error && <Notice tone="unknown">{error}</Notice>}
  </article>;
}
