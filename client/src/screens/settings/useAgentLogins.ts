// SPDX-License-Identifier: Apache-2.0
import { useCallback, useEffect, useState } from "react";
import { AGENT_LOGINS_FEATURE } from "../../app/agentLoginsFeature";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { commandResult } from "../../host/commandResult";
import type { HostSession } from "../../host/session";
import type { AgentLoginAction, AgentLoginEvent, AgentLoginsView, LoginHow, LoginStatus } from "../../protocol/types";
import { useMay } from "../../shell/NotAllowed";

export { AGENT_LOGINS_FEATURE };

/** Signing a login's sites out of the agent browser can wait for it to start. */
const ACT_WAIT_MS = 60_000;

/** How long a shown password stays on screen. */
export const SHOW_MS = 30_000;

const cache = new WeakMap<HostSession, AgentLoginsView>();

export const STATUS_WORDS: Record<LoginStatus["status"], string> = {
  ready: "Ready", paused: "Paused", needs_attention: "Needs attention",
};
export const HOW_WORDS: Record<LoginHow, string> = {
  whenever: "Use whenever needed", ask_first: "Ask me first",
};
export const EVENT_WORDS: Record<AgentLoginEvent, string> = {
  accepted: "Turned on", added: "Added", edited: "Edited", password_set: "Password replaced",
  code_key_set: "2FA key saved", code_key_removed: "2FA key removed", shown: "Password shown",
  paused: "Paused", resumed: "Resumed", paused_all: "Paused all", resumed_all: "Resumed all",
  signed_out: "Signed the agent browser out", deleted: "Deleted", filled: "Filled",
  code_filled: "Code filled", code_asked: "Asked you for a code", code_given: "Code given",
  approved: "Allowed", refused: "Refused",
};

/** The letters a generated password draws from: no look-alikes such as l, 1, O and 0. */
const ALPHABET = "abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789-_!@#%";

/**
 * A password from this phone's secure random source: 20 characters, at least
 * one of each kind most sites ask for. It goes to the computer like a typed
 * one, and is shown so it can be saved in the owner's own manager too.
 */
export function generatePassword(length = 20, random: (n: number) => Uint32Array = (n) => crypto.getRandomValues(new Uint32Array(n))): string {
  const kinds = [/[a-z]/, /[A-Z]/, /[0-9]/, /[-_!@#%]/];
  for (;;) {
    // Rejection sampling keeps every character equally likely.
    const limit = Math.floor(0x1_0000_0000 / ALPHABET.length) * ALPHABET.length;
    let out = "";
    while (out.length < length) {
      for (const n of random(length)) {
        if (n < limit && out.length < length) out += ALPHABET[n % ALPHABET.length];
      }
    }
    if (kinds.every((k) => k.test(out))) return out;
  }
}

/** The sites typed into one field, one per line or separated by commas or
 * spaces. The computer reduces each to its host and says which it refuses. */
export function splitSites(text: string): string[] {
  return text.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean);
}

/** The computer's agent logins, asked again whenever it says they changed. */
export function useAgentLogins(active = true) {
  const { session, profile } = useApp();
  const link = useLink();
  const allowed = useMay("accounts");
  const supported = useHost((s) => !s.capabilities || s.capabilities.features.includes(AGENT_LOGINS_FEATURE));
  const changed = useHost((s) => s.agentLoginsRevision);
  const [view, setView] = useState<AgentLoginsView | null>(cache.get(session) ?? null);
  const [error, setError] = useState<string | null>(null);
  const [revision, refresh] = useState(0);
  const reload = useCallback(() => refresh((n) => n + 1), []);
  useEffect(() => {
    let cancelled = false;
    setView(cache.get(session) ?? null);
    if (!active || !allowed || !supported || link.state !== "online") return;
    void session.query({ kind: "agent_logins" }).then(({ view }) => {
      if (cancelled) return;
      cache.set(session, view); setView(view); setError(null);
    }).catch((e: unknown) => { if (!cancelled) setError(describeError(e, profile.name)); });
    return () => { cancelled = true; };
  }, [session, profile.name, link.state, active, allowed, supported, changed, revision]);
  /**
   * Sends one action and waits for the computer to carry it out. Throws its
   * answer, in words, if it refused or the action failed.
   */
  const act = useCallback(async (action: AgentLoginAction, label: string): Promise<void> => {
    try {
      const receipt = await session.command({ command: "agent_login", action }, { label });
      await commandResult(session, receipt, { waitMs: ACT_WAIT_MS });
    } catch (e) { reload(); throw new Error(describeError(e, profile.name)); }
    reload();
  }, [session, profile.name, reload]);
  /** The login's password, from the computer, which records that it was shown. */
  const password = useCallback(async (loginId: string): Promise<string> => {
    try {
      const { password } = await session.query({ kind: "agent_login_password", login_id: loginId });
      return password;
    } catch (e) { throw new Error(describeError(e, profile.name)); }
    finally { reload(); }
  }, [session, profile.name, reload]);
  return { view, error, allowed, supported, act, password, reload };
}
