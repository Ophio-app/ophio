// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { tasks } from "../dev/sim/scenario";
import type { AttentionItem, GithubItem, Task } from "../protocol/types";
import { approvalShownOf, needOf, prNeedOf, shownLabels } from "./githubNeed";
import { commits, prReviews, signingApproval } from "../dev/sim/reviews";

const item = (rest: Partial<GithubItem> = {}): GithubItem => ({
  number: 41, pr: false, title: "Back closes the app", url: "https://github.com/samrivera/tidewater-api/issues/41", author: "samrivera",
  author_writes: true, mine: true, draft: false, labels: [], updated_at: 0, ...rest,
});
const task = (rest: Partial<Task>, github: Partial<NonNullable<Task["github"]>> = {}): Task => ({
  ...tasks(0)[0]!, state: "completed", attention_count: 1, ...rest,
  github: { link: { type: "issue", number: 41 }, issue: 41, rounds: 0, ...github },
});
const alert = (body: string): AttentionItem => ({
  attention_id: "a", kind: "waiting_for_you", title: "Couldn’t reproduce #41", body, task_id: null, run_id: null, approval_id: null,
  created_at: 0, resolved_at: null, priority: 3,
});

it("says what the fixer found, in its own words, and that closing the issue is what's left", () => {
  const fixed = needOf(item({ status: "waiting_for_you" }), task({}, { stage: "not_reproduced", already_fixed: true, finding: "Fixed by PR #48." }), alert("older"));
  expect(fixed).toMatchObject({ kind: "fixed", pill: "Close it?", headline: "The agent found this already fixed", said: "Fixed by PR #48." });
  expect(fixed?.ask).toMatch(/close the issue/);
  // A task from before the finding was saved says what its alert said.
  expect(needOf(item({ status: "waiting_for_you" }), task({}, { stage: "not_reproduced" }), alert("Present in 8262de20.")))
    .toMatchObject({ kind: "not_reproduced", said: "Present in 8262de20." });
});

it("sends a stuck pull request, a question or a failure to the agent's task", () => {
  expect(needOf(item({ status: "waiting_for_you" }), task({}, { stage: "capped", pr: { number: 52 } as never }), null))
    .toMatchObject({ kind: "stuck", headline: "The agent’s pull request #52 is stuck" });
  expect(needOf(item({ status: "waiting_for_you" }), task({ state: "awaiting_decision", status_line: "Which one?" }), null))
    .toMatchObject({ kind: "question", pill: "Answer", said: "Which one?" });
  expect(needOf(item({ status: "waiting_for_you" }), task({ state: "failed" }), alert("The build broke.")))
    .toMatchObject({ kind: "failed", said: "The build broke." });
});

it("tells one the Sort or the person put under Needs you from one an agent checked before", () => {
  expect(needOf(item({ group: "needs_you", triage_note: "Paging or streaming?" }), null, null))
    .toMatchObject({ kind: "sorted", said: "Paging or streaming?" });
  expect(needOf(item({ group: "needs_you", grouped_by_you: true }), null, null)).toMatchObject({ kind: "held", pill: "On hold" });
  expect(needOf(item({ group: "needs_you", labels: ["agent-not-reproduced"] }), null, null)).toMatchObject({ kind: "checked_before" });
  // Put there by the person, it's theirs whatever an agent found.
  expect(needOf(item({ group: "needs_you", labels: ["agent-not-reproduced"], grouped_by_you: true }), null, null)).toMatchObject({ kind: "held" });
  // One an agent can take, or one being worked, asks nothing.
  expect(needOf(item({ group: "agent_can_take" }), null, null)).toBeNull();
  expect(needOf(item({ group: "needs_you", status: "agent_working" }), task({ state: "working", attention_count: 0 }), null)).toBeNull();
});

it("hides the labels Øphio keeps for itself", () => {
  expect(shownLabels(["agent", "bug", "agent-found", "agent-not-reproduced", "docs"])).toEqual(["bug", "docs"]);
});

it("a row whose review stopped names its approval too, and merging from it approves what it named", () => {
  const pr = { number: 9, url: "", state: "open" as const, draft: false, head: commits.signing, passed: 2, failed: 0, pending: 0, unresolved: 0, conflict: false };
  const row = item({ number: 9, pr: true, head: commits.signing });
  const stopped = task({}, { link: { type: "new_pr" }, rounds: 3, pr, review: prReviews.escalated, approval: signingApproval });
  const need = prNeedOf(row, stopped);
  expect(need?.kind).toBe("review_stopped");
  expect(need?.row).toMatch(/review stopped.*It affects updates, signing and the review rules themselves, so it needs your approval to merge\.$/);
  expect(approvalShownOf(row, stopped)).toEqual(signingApproval.areas);
  // Stopped with nothing to approve, or approval of an older commit: none shown.
  expect(approvalShownOf(row, task({}, { link: { type: "new_pr" }, rounds: 3, pr, review: prReviews.escalated }))).toEqual([]);
  expect(approvalShownOf(item({ number: 9, pr: true, head: "f".repeat(40) }), stopped)).toEqual([]);
});
