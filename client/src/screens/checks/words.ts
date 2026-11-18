// SPDX-License-Identifier: Apache-2.0

import type { OphioCheckResult } from "../../protocol/generated/OphioCheckResult";
import type { ChecksDraftView } from "../../protocol/generated/ChecksDraftView";

/** Only result facts are worded here. Definition facts and differences always
 * come from the computer, never from an agent's explanation of its own check. */
export function checkOutcome(result: OphioCheckResult): string {
  switch (result.outcome.kind) {
    case "passed": return "Passed";
    case "failed": return "Failed";
    case "timed_out": return "Ran out of time";
    case "could_not_run": return "Could not run";
    case "skipped_paths": return "Not needed for these changes";
    case "off": return "Off";
    case "stopped": return "Not checked: stopped";
  }
}

export function checkDuration(seconds: number): string {
  const whole = Math.max(0, Math.round(seconds));
  const minutes = Math.floor(whole / 60), rest = whole % 60;
  return [minutes ? `${minutes} minute${minutes === 1 ? "" : "s"}` : "", rest || !minutes ? `${rest} second${rest === 1 ? "" : "s"}` : ""].filter(Boolean).join(" ");
}

export function suggestionTitle(view: ChecksDraftView): string {
  const items = view.draft.items;
  if (items.length === 1 && items[0]?.kind === "change") return `Your agent suggests a change to ${items[0].check.name}`;
  if (items.length === 1 && items[0]?.kind === "remove") return "Your agent suggests removing a check";
  return `Your agent suggests ${items.length === 1 ? "a check" : `${items.length} checks`}`;
}

export function proofWords(result: OphioCheckResult): string {
  if (result.outcome.kind === "passed") return `Passed on the project as it is now, in ${checkDuration(result.duration_secs)}`;
  if (result.outcome.kind === "failed") return "Fails on the project as it is now";
  if (result.outcome.kind === "could_not_run") return `Could not run on the project as it is now: ${result.outcome.reason}`;
  return `${checkOutcome(result)} on the project as it is now`;
}
