// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../brand";
import type { AttentionItem, Command, HarnessInfo, HarnessKind } from "../protocol/types";
import { openAttention, type HostState } from "../state/projection";

/** The computer's word that its agents can be signed in on the phone. An
 * older computer refuses `on_phone` and `submit_sign_in_code`, so neither is
 * ever sent to one. */
export const SIGN_IN_ON_PHONE_FEATURE = "sign_in_on_phone_v1";
export const SIGN_IN_ON_PHONE_UPDATE = `Update ${PRODUCT_NAME} on the computer to sign in from this phone.`;

/** Sign-ins asked for by a tap that opens the sign-in screen, which starts
 * them once it is shown on that computer: always, for Sign in again; only
 * while the agent is still signed out, for a notification tapped perhaps
 * long after. Kept a short while only, so a switch the person turned down
 * never starts one later. */
export type StartWhen = "always" | "if_signed_out";
const starts = new Map<string, { accountId: string; when: StartWhen; at: number }>();
const START_WAIT_MS = 30_000;

export function startWhenShown(profileId: string, accountId: string, when: StartWhen = "always"): void {
  starts.set(profileId, { accountId, when, at: Date.now() });
}

/** Whether the screen for `accountId` should start its sign-in now, once. */
export function takeStart(profileId: string, accountId: string): StartWhen | null {
  const asked = starts.get(profileId);
  if (!asked || asked.accountId !== accountId) return null;
  starts.delete(profileId);
  return Date.now() - asked.at < START_WAIT_MS ? asked.when : null;
}

/** A computer's notice that an agent isn't signed in: no task, an account. */
export function isSignInItem(item: AttentionItem): boolean {
  return !!item.account_id && !item.task_id;
}
export const signInAttention = (s: HostState): AttentionItem[] => openAttention(s).filter(isSignInItem);

/** The account behind each agent that can sign in on the phone. */
export const HARNESS_ACCOUNT: Partial<Record<HarnessKind, string>> = { claude_code: "claude", codex: "codex" };

/** Short names, as the computer's notice and notification name them. */
const SHORT: Record<string, string> = { claude: "Claude", codex: "Codex" };
export function shortService(accountId: string, fallback?: string): string {
  return SHORT[accountId] ?? fallback ?? accountId;
}

/** "Claude isn’t signed in on Studio", as the notice says it. */
export function signedOutLine(accountId: string, computer: string): string {
  return `${shortService(accountId)} isn’t signed in on ${computer}`;
}

/** Why a signed-out agent can't start: the computer's own remedy, or, where
 * this phone can sign it in (`computer` given), that it can do so. */
export function signedOutWords(h: HarnessInfo, computer?: string | null): string {
  if (h.account.state !== "signed_out") return "";
  const id = HARNESS_ACCOUNT[h.kind];
  return computer && id ? `${signedOutLine(id, computer)}. Sign in again from this phone first.` : h.account.remedy;
}

/** The agent account a command found signed out, as `account_not_ready`
 * says, where this phone can sign it in again; else null. */
export function signInForCommand(body: Command, state: Pick<HostState, "capabilities" | "grants" | "tasks">): string | null {
  if (!state.capabilities?.features.includes(SIGN_IN_ON_PHONE_FEATURE) || state.grants?.accounts === false) return null;
  const harness = body.command === "create_task" ? body.harness
    : "task_id" in body && typeof body.task_id === "string" ? state.tasks[body.task_id]?.harness : undefined;
  return harness ? HARNESS_ACCOUNT[harness] ?? null : null;
}
