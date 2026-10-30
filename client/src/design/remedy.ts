// SPDX-License-Identifier: Apache-2.0
import { PROGRAM_NAME } from "../brand";
import type { CheckStatus } from "../protocol/types";

/** The computer reports some remedies as a bare command for its own terminal.
 * Read on this device, they need to say where to run it. */
export function remedyText(remedy: string): string {
  return remedy.startsWith(`${PROGRAM_NAME} `)
    ? `On the computer, run: ${remedy}`
    : remedy;
}

const RUN = new RegExp(`(?:^|On the computer, run: |Run )(${PROGRAM_NAME} [^,;]+?)(?: on the computer|\\.?$)`);

/** A command for the computer's terminal inside a remedy, split from the
 * words around it so it can be shown on its own and copied. */
export function commandIn(text: string): { lead: string; command: string } | null {
  const m = text.match(RUN);
  if (!m) return null;
  const lead = text.replace(m[0], "On the computer, run the command below").trim();
  return { lead: /[.!?:]$/.test(lead) ? lead : `${lead}.`, command: m[1] ?? "" };
}

/** A readiness check's result in words. */
export const CHECK_WORD: Record<CheckStatus, string> = {
  passed: "ready",
  failed: "not ready",
  not_run: "not checked",
  unknown: "unknown",
};
