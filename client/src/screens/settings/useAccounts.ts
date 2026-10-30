// SPDX-License-Identifier: Apache-2.0
import { useCallback, useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { commandResult } from "../../host/commandResult";
import type { HostSession } from "../../host/session";
import type { Platform } from "../../host/link";
import type { AccountAction, AccountEvent, AccountKind, AccountState, AccountsView, PasswordManager, SignInStage } from "../../protocol/types";
import { useMay } from "../../shell/NotAllowed";

/** The computer's word that it manages accounts. */
export const ACCOUNTS_FEATURE = "accounts_v1";

/** A sign-out can wait for a sign-in to stop, then for two of the tool's own
 * commands, before the computer says how it went. */
const ACT_WAIT_MS = 90_000;

const cache = new WeakMap<HostSession, AccountsView>();
/** The sign-in this phone started and is following, per computer. */
const following = new WeakMap<HostSession, string>();

export const STATE_WORDS: Record<AccountState["state"], string> = {
  active: "Active", paused: "Paused", signed_out: "Signed out", needs_attention: "Needs attention",
};
export const KIND_WORDS: Record<AccountKind, string> = {
  agent: "Agent sign-in", cli: "CLI sign-in", oauth: "OAuth app", api_key: "API key",
};
export const EVENT_WORDS: Record<AccountEvent, string> = {
  signed_in: "Signed in", sign_in_failed: "Sign-in failed", paused: "Paused", resumed: "Resumed", paused_all: "Paused all",
  signed_out: "Signed out", deleted: "Deleted", key_set: "Key set", service_added: "Service added",
};
export const MANAGERS: [PasswordManager, string][] = [
  ["bitwarden", "Bitwarden"], ["one_password", "1Password"], ["proton_pass", "Proton Pass"],
  ["dashlane", "Dashlane"], ["last_pass", "LastPass"], ["kee_pass_xc", "KeePassXC"],
];

export function finished(stage: SignInStage): boolean {
  return stage.stage === "done" || stage.stage === "failed" || stage.stage === "cancelled" || stage.stage === "timed_out";
}

/**
 * Asks for the phone's own unlock before an account action. Without a way to
 * ask, the action doesn't go ahead: a phone with no secure screen lock refuses.
 */
export async function confirmHere(platform: Platform, title: string): Promise<void> {
  const lock = platform.appLock;
  if (!lock?.confirm) throw new Error("This phone can’t confirm account actions, so they aren’t sent.");
  await lock.confirm(title);
}

/** Whether this phone can ask for its own unlock before account actions. */
export function useConfirmable(): boolean | null {
  const { platform } = useApp();
  const [able, setAble] = useState<boolean | null>(null);
  useEffect(() => {
    let alive = true;
    const lock = platform.appLock;
    if (!lock?.confirm) { setAble(false); return; }
    void lock.state().then((s) => { if (alive) setAble(s.available); }, () => { if (alive) setAble(false); });
    return () => { alive = false; };
  }, [platform]);
  return able;
}

/** The computer's accounts, asked again whenever it says they changed. */
export function useAccounts(active = true) {
  const { session, profile } = useApp();
  const link = useLink();
  const allowed = useMay("accounts");
  const supported = useHost((s) => !s.capabilities || s.capabilities.features.includes(ACCOUNTS_FEATURE));
  const changed = useHost((s) => s.accountsRevision);
  const [view, setView] = useState<AccountsView | null>(cache.get(session) ?? null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [revision, refresh] = useState(0);
  const [followed, setFollowed] = useState(() => following.get(session) ?? null);
  const reload = useCallback(() => refresh((n) => n + 1), []);
  useEffect(() => {
    let cancelled = false;
    setView(cache.get(session) ?? null);
    if (!active || !allowed || !supported || link.state !== "online") return;
    setLoading(true);
    void session.query({ kind: "accounts" }).then(({ view }) => {
      if (cancelled) return;
      cache.set(session, view); setView(view); setError(null);
    }).catch((e: unknown) => { if (!cancelled) setError(describeError(e, profile.name)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [session, profile.name, link.state, active, allowed, supported, changed, revision]);
  /**
   * Sends one action and waits for the computer to carry it out. Throws its
   * answer, in words, if it refused or the action failed.
   */
  const act = useCallback(async (action: AccountAction, label: string): Promise<void> => {
    try {
      const receipt = await session.command({ command: "account", action }, { label });
      await commandResult(session, receipt, { waitMs: ACT_WAIT_MS });
    } catch (e) { reload(); throw new Error(describeError(e, profile.name)); }
    if (action.action === "sign_in") { following.set(session, action.account_id); setFollowed(action.account_id); }
    reload();
  }, [session, profile.name, reload]);
  const unfollow = useCallback(() => { following.delete(session); setFollowed(null); }, [session]);
  return { view, error, loading, allowed, supported, act, reload, followed, unfollow };
}
