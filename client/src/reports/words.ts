// SPDX-License-Identifier: Apache-2.0
import type { DeclineReason, ExpectedOption, IntakeLink, QuestionChoice, QuestionTemplate, ReportProgress, ReportSummary } from "../protocol/types";

export const declineWords: Record<DeclineReason, string> = {
  works_as_documented: "This is how Ophio is meant to work today; we’ve noted it as a request.",
  feature_request: "Noted as a request for Ophio.",
  duplicate: "Another report covers this too. We’ll follow it here.",
  not_enough_information: "We couldn’t find enough to work from this time.",
  outside_ophio: "This seems to come from outside Ophio.",
};
export const expectedWords: Record<ExpectedOption, string> = {
  should_work: "It should have worked.", should_ask: "It should have asked me first.",
  should_not_notify: "It shouldn’t have notified me.", should_be_faster: "It should have been faster.", something_else: "Something else.",
};
export function progressWords(status: ReportProgress): string {
  switch (status.state) {
    case "queued": return "Saved here. It will go when it can.";
    case "received": return "Received — we have your report.";
    case "known": return status.fixed_in ? `Known problem. Fixed in build ${status.fixed_in}.` : "Known problem. We’re following it.";
    case "question": return "A question is waiting for you.";
    case "reproduced": return "Reproduced — we can make it happen too.";
    case "not_reproduced": return "We haven’t been able to make it happen yet.";
    case "fix_written": return "A fix is written. It needs checking.";
    case "in_review": return "The fix is being checked.";
    case "fixed": return `Fixed in build ${status.build}.`;
    case "shipped": return `The fix is available in build ${status.build}.`;
    case "declined": return declineWords[status.reason];
    case "withdrawn": return "This report was withdrawn. Removal of any sent copy may still be pending.";
  }
}
export function questionWords(t: QuestionTemplate): string {
  switch (t.template) {
    case "still_happens": return `Does it still happen in build ${t.build}?`;
    case "happened_again": return "Has it happened again since you reported it?";
    case "after_update": return "Did it start after an update?";
    case "may_we_see": return `Can we see ${t.part === "task_transcript" ? "this task’s conversation" : "the real names for the pseudonyms"}?`;
    case "expected": return "Which comes closest to what you expected?";
  }
}
const choices: Record<Exclude<QuestionChoice["choice"], "option">, string> = { yes: "Yes", no: "No", not_sure: "Not sure", allow: "Allow", deny: "Don’t allow" };
export const choiceWords = (c: QuestionChoice): string => c.choice === "option" ? expectedWords[c.option] : choices[c.choice];
export function questionChoices(t: QuestionTemplate): QuestionChoice[] {
  if (t.template === "expected") return t.options.map(option => ({ choice: "option", option }));
  return (t.template === "may_we_see" ? ["allow", "deny"] as const : ["yes", "no", "not_sure"] as const).map(choice => ({ choice }));
}
export const latestProgress = (r: ReportSummary): ReportProgress => r.history.at(-1)?.status ?? { state: "queued" };
export function intakeWords(link: IntakeLink): string | null {
  switch (link.state) {
    case "reachable": return null;
    case "none": return "This computer has nowhere to send reports yet, so they wait here.";
    case "paused": return "Reports aren’t being taken right now. They’ll go when that reopens.";
    case "unreachable": return `The place reports go can’t be reached. ${link.queued} report${link.queued === 1 ? " waits" : "s wait"} here.`;
  }
}
export function fixedBuild(status: ReportProgress): string | undefined {
  return status.state === "fixed" || status.state === "shipped" ? status.build : status.state === "known" ? status.fixed_in : undefined;
}
export function reportAbout(r: ReportSummary): string {
  const screen = r.signature.screen.split("/")[1]?.replace(/_/g, " ") || "app";
  const error = r.signature.code;
  const what = r.origin.type === "crash" ? "Closed unexpectedly" : error === "none" ? "Something didn’t work as expected" : error === "permission_denied" ? "An action wasn’t allowed" : error === "stale_revision" ? "Something changed before the action finished" : "An action couldn’t finish";
  return `${screen.charAt(0).toUpperCase()}${screen.slice(1)} · ${what}`;
}
