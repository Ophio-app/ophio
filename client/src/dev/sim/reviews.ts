// SPDX-License-Identifier: Apache-2.0
//
// Review results as the computer records them (`crates/protocol/src/review.rs`):
// one for each way a pull request's review can end, for the sim's pull
// requests and for tests of how the phone words them.

import type { ApprovalNeed, LaunchChoice, PrReview, ReviewFinding, ReviewRulesUsed, TaskReview } from "../../protocol/types";

const MIN = 60_000;
export const reviewer: LaunchChoice = { harness: "claude_code", model: "claude-opus-5-5", effort: "high" };
export const verifier: LaunchChoice = { harness: "codex", model: "gpt-6-astra", effort: "high" };

/** Commits the fixtures review, newest last. */
export const commits = {
  first: "1f2e3d4c5b6a79880716253443526170f8e9dab0",
  corrected: "5b7e2c9d41aa0f3e8b1c2d4e5f60718293a4b5c6",
  signing: "8c4d2e1f00aa77bb66cc55dd44ee33ff22110099",
  exportFirst: "aa11bb22cc33dd44ee55ff6600778899aabbccdd",
  exportNow: "d00dfeed11223344556677889900aabbccddeeff",
} as const;

export const rules: ReviewRulesUsed = { revision: "0a9b8c7d6e5f40312233445566778899aabbccdd", source: "origin/main", files: ["REVIEW.md", "src/orders/REVIEW.md"] };
/** The change edits REVIEW.md, so its edits were ignored and main's rules followed. */
export const rulesIgnored: ReviewRulesUsed = { ...rules, files: ["REVIEW.md"], ignored_changes: true };
export const noRules: ReviewRulesUsed = { revision: rules.revision, source: "origin/main", missing: "The project has no REVIEW.md." };

const finding = (f: Partial<ReviewFinding> & Pick<ReviewFinding, "title">): ReviewFinding => ({ disposition: { state: "open" }, round: 1, ...f });

/** Two corrected after a recheck, one set aside as intended. */
export const correctedFindings: ReviewFinding[] = [
  finding({
    title: "Reset is sent as a timestamp, not seconds", path: "src/orders/limits.rs", line: 58, severity: "medium",
    scenario: "Call GET /v1/orders twice within a second.", expected: "X-RateLimit-Reset counts the seconds until the window ends, as the other endpoints do.",
    actual: "It holds a Unix timestamp, so clients wait decades.", verdict: "confirmed", evidence: "reproduced",
    reason: "A test calling the endpoint twice saw Reset: 1792000000.", revision: commits.first, disposition: { state: "corrected" },
  }),
  finding({
    title: "Remaining can go below zero under concurrent requests", path: "src/orders/limits.rs", line: 71, severity: "high",
    scenario: "Two requests arrive together when one call is left.", expected: "One succeeds and Remaining reads 0.", actual: "Both read the counter first, so Remaining reads -1.",
    verdict: "confirmed", evidence: "source_reasoning", reason: "The counter is read and written in two steps without the limiter's lock.",
    revision: commits.first, disposition: { state: "corrected" },
  }),
  finding({
    title: "Headers are missing on 429 responses", path: "src/orders/routes.rs", line: 22, severity: "low",
    verdict: "rejected", reason: "429 responses come from the shared limiter middleware, which already sets them; this change doesn't touch it.",
    revision: commits.first, disposition: { state: "set_aside" },
  }),
];

/** Everything a pull request's review can say, by how it ended. */
export const prReviews = {
  /** Nothing found, nothing set aside. */
  clean: {
    round: 1, reviewed_head: commits.corrected, rules, reviewer, verifier,
  } satisfies PrReview,
  /** Two confirmed, corrected, and the corrections rechecked in round two. */
  corrected: {
    round: 2, reviewed_head: commits.corrected, findings: correctedFindings, rules, reviewer, verifier,
    not_run: ["the Android screen check"],
  } satisfies PrReview,
  /** A corrected problem came back, so the review stopped for the person. */
  escalated: {
    round: 3, reviewed_head: commits.exportNow, rules, reviewer, verifier,
    escalated: "A problem marked corrected in round two came back in round three: exports over 5,000 orders still stop at the first page.",
    findings: [
      finding({
        title: "Large exports stop after the first page", path: "src/export/pager.rs", line: 40, severity: "high",
        scenario: "Export an account with 12,000 orders.", expected: "All 12,000 rows.", actual: "Only the first 5,000.",
        verdict: "confirmed", evidence: "reproduced", reason: "Ran the export against the 12,000-order fixture.", revision: commits.exportNow, round: 3, disposition: { state: "fixing" },
      }),
    ],
  } satisfies PrReview,
  /** Clean, but touching updates and signing; the change also edits the review rules. */
  approval: {
    round: 1, reviewed_head: commits.signing, rules: rulesIgnored, reviewer, verifier,
  } satisfies PrReview,
  /** Nothing confirmed: both possible problems were set aside with reasons. */
  setAside: {
    round: 1, reviewed_head: commits.corrected, rules: noRules, reviewer, verifier,
    findings: [
      finding({ title: "Retry loop has no upper bound", path: "src/client/retry.rs", line: 12, verdict: "rejected", reason: "It stops after five tries: the bound is in the caller.", disposition: { state: "set_aside" } }),
      finding({ title: "Token may be logged on failure", path: "src/client/auth.rs", line: 88, verdict: "uncertain", reason: "The logger's redaction couldn't be run here, so whether it hides the token is unknown.", disposition: { state: "set_aside" } }),
    ],
  } satisfies PrReview,
} as const;

/** The approval a change touching updates and signing waits for. */
export const signingApproval: ApprovalNeed = {
  areas: [{ area: "updates" }, { area: "signing" }, { area: "review_policy" }],
  paths: ["src/update/apply.rs", "release/sign.sh", "REVIEW.md"],
  head: commits.signing,
};

/** A finder run's own task: one filed as an issue, one set aside. */
export const finderReview: TaskReview = {
  role: "find", rules,
  findings: [
    finding({ title: "Webhook retries skip the signature check", path: "src/webhooks/retry.rs", line: 31, severity: "high", verdict: "confirmed", evidence: "reproduced", reason: "A replayed request with a bad signature was accepted on retry.", disposition: { state: "filed", number: 231 } }),
    finding({ title: "Order totals round half down", path: "src/orders/money.rs", line: 9, severity: "medium", verdict: "rejected", reason: "Banker's rounding is intended and documented in money.rs.", disposition: { state: "set_aside" } }),
  ],
};

/** When each fixture's review started and last changed, against the sim's clock. */
export const stamped = (review: PrReview, now: number): PrReview => ({ ...review, started_at: now - 70 * MIN, updated_at: now - 55 * MIN });
