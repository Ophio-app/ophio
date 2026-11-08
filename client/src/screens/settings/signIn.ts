// SPDX-License-Identifier: Apache-2.0
//
// Signing an agent in again from the phone: the computer starts the agent's
// own sign-in, and the phone's browser finishes it, so a lapsed sign-in can
// be fixed while away from the computer and with no model connected.

import { useApp, useHost } from "../../app/context";
import type { HarnessKind, SignInStage } from "../../protocol/types";
import type { HostState } from "../../state/projection";
import { useMay } from "../../shell/NotAllowed";
import { HARNESS_ACCOUNT, SIGN_IN_ON_PHONE_FEATURE } from "../../app/signInFeature";

export { HARNESS_ACCOUNT, SIGN_IN_ON_PHONE_FEATURE, isSignInItem, shortService, signInAttention, signedOutLine, signedOutWords, startWhenShown, takeStart } from "../../app/signInFeature";

export const hasPhoneSignIn = (s: Pick<HostState, "capabilities">): boolean =>
  !!s.capabilities?.features.includes(SIGN_IN_ON_PHONE_FEATURE);

/** Whether this phone can sign the computer's agents in itself: the computer
 * offers it and this device may manage its accounts. */
export function usePhoneSignIn(): boolean {
  const supported = useHost(hasPhoneSignIn);
  const allowed = useMay("accounts");
  return supported && allowed;
}

/** The account to sign in again for a signed-out agent, when this phone can. */
export function useSignInOffer(harness: HarnessKind | null | undefined): string | null {
  const able = usePhoneSignIn();
  const id = harness ? HARNESS_ACCOUNT[harness] : undefined;
  return able && id ? id : null;
}

/** The computer's name, where this phone can sign its agents in itself;
 * else null, and blockers keep the computer's own remedy. */
export function useSignInOn(): string | null {
  const able = usePhoneSignIn();
  const name = useApp().profile.name;
  return able ? name : null;
}

/** The stage the phone finishes in its own browser. */
export type OnPhoneStage = Extract<SignInStage, { stage: "on_phone" }>;

/** The words for the button that fixes a problem page, by its site. */
export function helpLabel(url: string): string {
  try {
    const host = new URL(url).hostname;
    if (host === "chatgpt.com" || host.endsWith(".chatgpt.com")) return "Open ChatGPT’s security settings";
    return `Open ${host.replace(/^www\./, "")}`;
  } catch {
    return "Open the help page";
  }
}

/** Only web pages leave the app, and only to the phone's browser. */
export function webLink(url: string | undefined | null): string | null {
  if (!url) return null;
  try { return new URL(url).protocol === "https:" ? url : null; } catch { return null; }
}
