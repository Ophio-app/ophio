// SPDX-License-Identifier: Apache-2.0
//
// "Sign in again", wherever the phone learns an agent isn't signed in: it
// opens the sign-in on the computer it is about, which starts it there and
// finishes it in this phone's browser. In another computer's part of All,
// the tap switches to that computer first, as opening any of its items does.

import type { ReactNode } from "react";
import { KeyRound, ShieldCheck } from "lucide-react";
import { describeError, shallowEqual, useApp, useHost, useLink } from "../../app/context";
import { commandResult } from "../../host/commandResult";
import { settle } from "../../host/expected";
import { toast } from "../../design/Toast";
import type { AttentionItem } from "../../protocol/types";
import { Button, type ButtonSize, type ButtonVariant } from "../../design/Button";
import { Notice } from "../../design/Notice";
import type { HarnessKind } from "../../protocol/types";
import { signInAttention, signedOutLine, startWhenShown, usePhoneSignIn, useSignInOffer } from "./signIn";
import "./signIn.css";

/** `leave` closes what the button sits in, as a sheet, before the sign-in opens. */
export function SignInAgainButton({ accountId, size = "sm", variant = "primary", block, label = "Sign in again", leave }: {
  accountId: string; size?: ButtonSize; variant?: ButtonVariant; block?: boolean; label?: string; leave?: () => void;
}) {
  const { profile, ui } = useApp();
  return (
    <Button size={size} variant={variant} block={block} icon={ShieldCheck}
      onClick={() => { leave?.(); startWhenShown(profile.profileId, accountId); ui.push({ screen: "sign_in", accountId }); }}>
      {label}
    </Button>
  );
}

/** Puts this lapse's notice away: gone at once, back with the computer's
 * words if it refuses. The next lapse raises a new one. */
export function NotNowButton({ item, size = "sm", block, then }: { item: AttentionItem; size?: ButtonSize; block?: boolean; then?: () => void }) {
  const { session, profile } = useApp();
  const online = useLink().state === "online";
  const dismiss = async () => {
    const id = item.attention_id;
    const drop = session.state.expect((s) => {
      const a = s.attention[id];
      return a ? { ...s, attention: { ...s.attention, [id]: { ...a, resolved_at: a.resolved_at ?? Date.now() } } } : s;
    });
    then?.();
    try {
      await commandResult(session, await session.command({ command: "account", action: { action: "dismiss_sign_in_notice", account_id: item.account_id! } }, { label: "Putting away the sign-in notice" }));
    } catch (e) {
      drop();
      toast(describeError(e, profile.name), { tone: "stop" });
      return;
    }
    settle(session.state, drop, (s) => !s.attention[id] || s.attention[id]!.resolved_at !== null);
  };
  return <Button size={size} block={block} variant="quiet" disabled={!online} onClick={() => void dismiss()}>Not now</Button>;
}

/** An agent's "not signed in" line with Sign in again, where the computer
 * can sign it in on this phone; `fallback`, today's words, where it can't. */
export function SignedOut({ harness, fallback, tone = "warn", leave }: { harness: HarnessKind | null | undefined; fallback: ReactNode; tone?: "warn" | "unknown" | "info"; leave?: () => void }) {
  const { profile } = useApp();
  const offer = useSignInOffer(harness);
  if (!offer) return <>{fallback}</>;
  return (
    <Notice tone={tone} icon={KeyRound} title={signedOutLine(offer, profile.name)} action={<SignInAgainButton accountId={offer} leave={leave} />}>
      Sign in again from this phone. The sign-in page opens here.
    </Notice>
  );
}

/** The computer's notices that an agent isn't signed in, each with Sign in
 * again, for Needs you. Each names its computer. */
export function SignInNeedsYou() {
  const items = useHost(signInAttention, shallowEqual);
  const able = usePhoneSignIn();
  if (!items.length) return null;
  return <>
    {items.map((item) => (
      <Notice key={item.attention_id} tone="warn" icon={KeyRound} className="sign-in-needs-you" report={false} title={item.title}
        action={able && item.account_id ? <div className="sign-in-notice__actions"><SignInAgainButton accountId={item.account_id} /><NotNowButton item={item} /></div> : undefined}>
        {item.body}
      </Notice>
    ))}
  </>;
}
