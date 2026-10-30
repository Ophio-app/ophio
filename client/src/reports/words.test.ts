// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { DeclineReason, ExpectedOption, IntakeLink, QuestionChoice, QuestionTemplate, ReportProgress } from "../protocol/types";
import { choiceWords, declineWords, expectedWords, intakeWords, progressWords, questionChoices, questionWords } from "./words";

it("words every report status, decline, question and answer", () => {
  const statuses: Record<ReportProgress["state"], ReportProgress> = {
    queued: { state: "queued" }, received: { state: "received" }, known: { state: "known", fixed_in: "next" }, question: { state: "question" }, reproduced: { state: "reproduced" }, not_reproduced: { state: "not_reproduced" }, fix_written: { state: "fix_written" }, in_review: { state: "in_review" }, fixed: { state: "fixed", build: "next" }, shipped: { state: "shipped", build: "next" }, declined: { state: "declined", reason: "works_as_documented" }, withdrawn: { state: "withdrawn" },
  };
  for (const status of Object.values(statuses)) expect(progressWords(status).length).toBeGreaterThan(10);
  for (const reason of Object.keys(declineWords) as DeclineReason[]) expect(progressWords({ state: "declined", reason })).toBe(declineWords[reason]);
  const templates: Record<QuestionTemplate["template"], QuestionTemplate> = {
    still_happens: { template: "still_happens", build: "next" }, happened_again: { template: "happened_again" }, after_update: { template: "after_update" }, may_we_see: { template: "may_we_see", part: "task_transcript" }, expected: { template: "expected", options: Object.keys(expectedWords) as ExpectedOption[] },
  };
  const choices = new Set<QuestionChoice["choice"]>();
  for (const template of Object.values(templates)) {
    expect(questionWords(template)).toMatch(/\?$/);
    for (const choice of questionChoices(template)) { choices.add(choice.choice); expect(choiceWords(choice).length).toBeGreaterThan(1); }
  }
  expect([...choices].sort()).toEqual(["allow", "deny", "no", "not_sure", "option", "yes"]);
  expect(questionWords({ template: "may_we_see", part: "names" })).toContain("real names");
  expect(progressWords({ state: "known" })).toContain("Known problem");
});

it("says where reports wait without the word intake", () => {
  const links: IntakeLink[] = [{ state: "none" }, { state: "paused" }, { state: "unreachable", since: 0, queued: 2 }];
  for (const link of links) expect(intakeWords(link)).not.toMatch(/intake/i);
  expect(intakeWords({ state: "reachable" })).toBeNull();
  expect(progressWords({ state: "withdrawn" })).not.toMatch(/intake/i);
});

// The computer marks a report withdrawn before an offline server can delete it.
it("does not claim remote deletion has finished when withdrawal is queued", () => {
  expect(progressWords({ state: "withdrawn" })).toBe("This report was withdrawn. Removal of any sent copy may still be pending.");
});
