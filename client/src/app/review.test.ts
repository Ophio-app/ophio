// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import { commits, finderReview, noRules, prReviews, rules, rulesIgnored, signingApproval } from "../dev/sim/reviews";
import type { ApprovalNeed, FindingDisposition, PrReview, ReviewFinding, Task } from "../protocol/types";
import {
  approvalSentence, areaWords, counted, listWords, numberWord, prNeedLine, reviewResult, rulesWords, taskReviewInput, uncheckedReasons, verdictWords,
  type ChecksCount, type ReviewResultInput,
} from "./review";

const HEAD = "abcdef0123456789abcdef0123456789abcdef01";
const passed: ChecksCount = { passed: 3, failed: 0, pending: 0 };
const input = (over: Partial<ReviewResultInput> = {}): ReviewResultInput => ({
  review: { round: 1, reviewed_head: HEAD }, approval: null, head: HEAD, checks: passed, state: "open", ...over,
});
const headline = (over: Partial<ReviewResultInput> = {}) => reviewResult(input(over)).headline.join(" ");
const found = (n: number, f: Partial<ReviewFinding>, disposition: FindingDisposition = { state: "open" }): ReviewFinding[] =>
  Array.from({ length: n }, (_, i) => ({ title: `Problem ${i + 1}`, round: 1, verdict: "confirmed", evidence: "reproduced", ...f, disposition }));
const reviewed = (findings: ReviewFinding[], more: Partial<PrReview> = {}): PrReview => ({ round: 2, reviewed_head: HEAD, findings, ...more });

describe("numbers and lists", () => {
  it("writes small numbers as words and larger ones as figures", () => {
    expect([0, 1, 2, 9, 12, 13, 40].map(numberWord)).toEqual(["no", "one", "two", "nine", "twelve", "13", "40"]);
    expect(counted(1, "problem")).toBe("one problem");
    expect(counted(2, "problem")).toBe("two problems");
    expect(counted(1, "possible problem")).toBe("one possible problem");
  });
  it("joins lists the way a sentence does", () => {
    expect(listWords([])).toBe("");
    expect(listWords(["a"])).toBe("a");
    expect(listWords(["a", "b"])).toBe("a and b");
    expect(listWords(["a", "b", "c"])).toBe("a, b and c");
  });
});

describe("the headline", () => {
  it("says a clean review found no problems, never that the change is right", () => {
    const r = reviewResult(input());
    expect(r.headline.join(" ")).toBe("The review found no problems. The pull request’s checks passed.");
    expect(r.headline.join(" ")).not.toMatch(/correct|proved|safe|verified/i);
    expect(r.evidence.text).toBe("No problem was confirmed, so there was nothing to show. That doesn’t prove the change is right.");
    expect(r.needsYou).toBeNull();
  });

  it("says what wasn't run, even when nothing was found", () => {
    expect(headline({ review: { round: 1, reviewed_head: HEAD, not_run: ["the Android screen check."] } }))
      .toBe("The review found no problems. The pull request’s checks passed. One check was not run: the Android screen check.");
    expect(headline({ review: { round: 1, reviewed_head: HEAD, not_run: ["the Android screen check", "the Windows build", "the load test"] } }))
      .toBe("The review found no problems. The pull request’s checks passed. Three checks were not run: the Android screen check, the Windows build and the load test.");
  });

  it("reads the design's example as written", () => {
    expect(headline({ review: prReviews.corrected, head: commits.corrected, approval: { ...signingApproval, areas: [{ area: "updates" }, { area: "permissions" }], head: commits.corrected } }))
      .toBe("Two confirmed problems were corrected. The corrections were reviewed and the pull request’s checks passed. One check was not run: the Android screen check. This change affects updates and permissions, so it needs your approval.");
  });

  it.each([
    [1, "One confirmed problem was corrected."],
    [2, "Two confirmed problems were corrected."],
    [11, "Eleven confirmed problems were corrected."],
    [14, "14 confirmed problems were corrected."],
  ])("counts %i corrected problem(s) in words", (n, said) => {
    expect(reviewResult(input({ review: reviewed(found(n, {}, { state: "corrected" })) })).headline[0]).toBe(said);
  });

  it("says what became of each confirmed problem, the first with its noun and the rest as more", () => {
    const findings = [
      ...found(2, {}, { state: "corrected" }), ...found(1, {}, { state: "fixing" }),
      ...found(1, {}, { state: "filed", number: 31 }), ...found(3, {}, { state: "open" }),
    ];
    expect(reviewResult(input({ review: reviewed(findings) })).headline.slice(0, 4)).toEqual([
      "Two confirmed problems were corrected.", "One more is being fixed.", "One more was filed as issue #31.", "Three more are still open.",
    ]);
    expect(reviewResult(input({ review: reviewed(found(1, {}, { state: "open" })) })).headline[0]).toBe("One confirmed problem is still open.");
    expect(reviewResult(input({ review: reviewed(found(2, {}, { state: "fixing" })) })).headline[0]).toBe("Two confirmed problems are being fixed.");
    const filed = [...found(1, {}, { state: "filed", number: 40 }), ...found(1, {}, { state: "filed", number: 41 }), ...found(1, {}, { state: "filed", number: 42 })];
    expect(reviewResult(input({ review: reviewed(filed) })).headline[0]).toBe("Three confirmed problems were filed as issues #40, #41 and #42.");
  });

  it("only says the corrections were reviewed when some were corrected, and keeps the checks a separate fact", () => {
    const corrected = reviewed(found(1, {}, { state: "corrected" }));
    expect(headline({ review: corrected })).toBe("One confirmed problem was corrected. The corrections were reviewed and the pull request’s checks passed.");
    expect(headline({ review: corrected, checks: { passed: 2, failed: 1, pending: 0 } }))
      .toBe("One confirmed problem was corrected. The corrections were reviewed, but one check on the pull request has failed.");
    expect(headline({ review: corrected, checks: { passed: 0, failed: 3, pending: 0 } }))
      .toBe("One confirmed problem was corrected. The corrections were reviewed, but three checks on the pull request have failed.");
    expect(headline({ review: corrected, checks: { passed: 1, failed: 0, pending: 2 } }))
      .toBe("One confirmed problem was corrected. The corrections were reviewed. The pull request’s checks are still running.");
    expect(headline({ review: corrected, checks: { passed: 0, failed: 0, pending: 0 } })).toBe("One confirmed problem was corrected. The corrections were reviewed.");
    expect(headline({ review: corrected, checks: null })).toBe("One confirmed problem was corrected. The corrections were reviewed.");
    expect(headline({ review: reviewed(found(1, {}, { state: "open" })), checks: { passed: 0, failed: 2, pending: 0 } }))
      .toBe("One confirmed problem is still open. Two checks on the pull request have failed.");
    expect(headline({ checks: null })).toBe("The review found no problems.");
  });

  it("says what a second agent set aside instead of claiming nothing was raised", () => {
    expect(headline({ review: prReviews.setAside, head: commits.corrected, checks: null })).toBe("The review found no confirmed problems. A second agent checked two possible problems and set them aside.");
    expect(headline({ review: reviewed([prReviews.setAside.findings[0]!]), checks: null }))
      .toBe("The review found no confirmed problems. A second agent checked one possible problem and set it aside.");
    // A confirmed one that wasn't acted on counts as set aside, not as found.
    expect(headline({ review: reviewed(found(1, {}, { state: "set_aside" })), checks: null })).toMatch(/^The review found no confirmed problems/);
  });

  it("says when findings are still being checked", () => {
    expect(headline({ review: reviewed(found(2, { verdict: undefined, evidence: undefined })), checks: null }))
      .toBe("A second agent is still checking two possible problems.");
    expect(headline({ review: reviewed([...found(1, {}, { state: "fixing" }), ...found(1, { verdict: undefined })]), checks: null }))
      .toBe("One confirmed problem is being fixed. A second agent is still checking one possible problem.");
  });

  it("says a review is under way, and when newer commits haven't been reviewed", () => {
    expect(headline({ review: { round: 0, reviewing_head: HEAD }, checks: null })).toBe("A review of this change is under way.");
    expect(headline({ review: { round: 0 }, checks: null })).toBe("No review of this change has been recorded.");
    expect(headline({ head: "1234567aaaa", checks: null })).toBe("The review found no problems. Newer commits haven’t been reviewed yet.");
    expect(headline({ review: { round: 1, reviewed_head: HEAD, reviewing_head: "1234567aaaa" }, head: "1234567aaaa", checks: null }))
      .toBe("The review found no problems. A review of the newest commit is under way.");
    // The newest commit as reviewed: nothing more to say.
    expect(reviewResult(input()).unreviewed).toBe(false);
    expect(reviewResult(input({ head: "1234567aaaa" })).unreviewed).toBe(true);
    // A merged pull request has no newer commits to worry about.
    expect(reviewResult(input({ head: "1234567aaaa", state: "merged" })).unreviewed).toBe(false);
  });

  it("says a stopped review needs the person, and how far it got", () => {
    const r = reviewResult(input({ review: prReviews.escalated, head: commits.exportNow, checks: { passed: 1, failed: 1, pending: 0 } }));
    expect(r.headline.join(" ")).toBe("One confirmed problem is being fixed. One check on the pull request has failed. The review stopped and needs you to decide what happens next.");
    // The reason itself is shown once, above the facts.
    expect(r.review).toEqual({ word: "Stopped", tone: "warn", text: "It stopped after three rounds and is waiting for you. The last one covered commit d00dfee." });
    expect(r.needsYou).toBe("escalated");
    // Once it's closed, it no longer needs anyone.
    expect(reviewResult(input({ review: prReviews.escalated, state: "closed" })).needsYou).toBeNull();
  });

  it("names the sensitive areas and asks for approval of the newest commit only", () => {
    const r = reviewResult(input({ review: prReviews.approval, approval: signingApproval, head: commits.signing, checks: { passed: 2, failed: 0, pending: 0 } }));
    expect(r.headline.join(" ")).toBe("The review found no problems. The pull request’s checks passed. This change affects updates, signing and the review rules themselves, so it needs your approval.");
    expect(r.approval).toMatchObject({ word: "Needs yours", tone: "warn" });
    expect(r.approval.text).toContain("Approving covers commit 8c4d2e1 only.");
    expect(r.needsYou).toBe("approval");
    expect(r.approvalHead).toBe(commits.signing);
    expect(r.checks.text).toBe("Both of its checks on GitHub passed.");
  });

  it("won't ask to approve a commit the reasons weren't for", () => {
    const r = reviewResult(input({ review: { round: 1, reviewed_head: commits.signing }, approval: signingApproval, head: HEAD, checks: null }));
    expect(r.headline.at(-1)).toBe("An older commit needed your approval. Whether the newest one does is checked when you merge it.");
    expect(r.approval.word).toBe("Out of date");
    expect(r.needsYou).toBeNull();
    expect(r.approvalHead).toBeNull();
    // Merged or closed, approval is moot.
    expect(reviewResult(input({ approval: { ...signingApproval, head: HEAD }, state: "merged" })).approval.word).toBe("Merged");
    expect(reviewResult(input({ approval: { ...signingApproval, head: HEAD }, state: "merged" })).headline.join(" ")).not.toContain("approval");
  });

  it("puts the escalation before the approval when both apply", () => {
    const r = reviewResult(input({ review: { ...prReviews.escalated, reviewed_head: HEAD }, approval: { ...signingApproval, head: HEAD } }));
    expect(r.needsYou).toBe("escalated");
    expect(r.headline.slice(-2)).toEqual([
      "The review stopped and needs you to decide what happens next.",
      "This change affects updates, signing and the review rules themselves, so it needs your approval.",
    ]);
  });
});

describe("the evidence", () => {
  it("never calls reading the code a test", () => {
    const read = reviewResult(input({ review: reviewed(found(1, { evidence: "source_reasoning" })) })).evidence;
    expect(read).toEqual({ word: "From reading the code", tone: "neutral", text: "The confirmed problem was found by reading the code. Nothing was run to show it." });
    expect(reviewResult(input({ review: reviewed(found(3, { evidence: "source_reasoning" })) })).evidence.text)
      .toBe("All three confirmed problems were found by reading the code. Nothing was run to show them.");
    expect(reviewResult(input({ review: reviewed(found(2, { evidence: "source_reasoning" })) })).evidence.text)
      .toBe("Both confirmed problems were found by reading the code. Nothing was run to show them.");
  });
  it("says which were shown by running them", () => {
    expect(reviewResult(input({ review: reviewed(found(1, {})) })).evidence).toEqual({ word: "Shown by running it", tone: "ok", text: "The confirmed problem was shown by running it." });
    expect(reviewResult(input({ review: reviewed(found(2, {})) })).evidence.text).toBe("Both confirmed problems were shown by running them.");
    expect(reviewResult(input({ review: reviewed([...found(2, {}), ...found(1, { evidence: "source_reasoning" })]) })).evidence)
      .toEqual({ word: "Mixed", tone: "neutral", text: "Two were shown by running them. One was found by reading the code, without running anything." });
    expect(reviewResult(input({ review: reviewed([...found(1, {}), ...found(1, { evidence: undefined }), ...found(2, { verdict: undefined })]) })).evidence.text)
      .toBe("One was shown by running it. One doesn’t say how it was shown. Two more possible problems are still being checked.");
  });
  it("says what's being checked before anything is confirmed", () => {
    expect(reviewResult(input({ review: reviewed(found(1, { verdict: undefined })) })).evidence.text)
      .toBe("A second agent is checking the possible problem before anything is done about it.");
    expect(reviewResult(input({ review: reviewed(found(3, { verdict: undefined })) })).evidence.text)
      .toBe("A second agent is checking the three possible problems before anything is done about them.");
  });
});

describe("the other facts", () => {
  it("says how far the review got", () => {
    expect(reviewResult(input()).review).toEqual({ word: "Done", tone: "ok", text: "Reviewed up to commit abcdef0. That took one round of at most three." });
    expect(reviewResult(input({ review: { round: 3, reviewed_head: HEAD }, head: "999999999" })).review.text)
      .toBe("Reviewed up to commit abcdef0. That took three rounds of at most three. Newer commits haven’t been reviewed.");
    expect(reviewResult(input({ review: { round: 1, reviewed_head: HEAD, reviewing_head: "1234567aaaa" } })).review)
      .toEqual({ word: "Under way", tone: "accent", text: "Reviewing commit 1234567, after one round so far." });
    expect(reviewResult(input({ review: { round: 0 } })).review).toEqual({ word: "Not yet", tone: "neutral", text: "No review of it has been recorded." });
  });
  it("says how the checks went on their own", () => {
    const checks = (c: ChecksCount | null) => reviewResult(input({ checks: c })).checks;
    expect(checks({ passed: 1, failed: 0, pending: 0 })).toEqual({ word: "Passed", tone: "ok", text: "Its one check on GitHub passed." });
    expect(checks({ passed: 5, failed: 2, pending: 1 })).toEqual({ word: "Two failed", tone: "stop", text: "Two checks on GitHub have failed." });
    expect(checks({ passed: 0, failed: 1, pending: 0 }).word).toBe("One failed");
    expect(checks({ passed: 2, failed: 0, pending: 1 })).toEqual({ word: "Running", tone: "accent", text: "Its checks on GitHub are still running." });
    expect(checks({ passed: 0, failed: 0, pending: 0 })).toEqual({ word: "None", tone: "neutral", text: "It has no checks on GitHub." });
    expect(checks(null).word).toBe("Unknown");
  });
  it("says approval isn't needed without claiming it's safe", () => {
    expect(reviewResult(input()).approval).toEqual({ word: "Not needed", tone: "neutral", text: "Nothing it changes needs your approval. You still choose when to merge it." });
  });
});

describe("approval words", () => {
  const need = (areas: ApprovalNeed["areas"]): ApprovalNeed => ({ areas, head: HEAD });
  it("names every area in plain words", () => {
    const all: ApprovalNeed["areas"] = [
      { area: "permissions" }, { area: "pairing_or_login" }, { area: "credentials" }, { area: "sandboxing" }, { area: "updates" }, { area: "signing" },
      { area: "release_or_build" }, { area: "installers" }, { area: "data_changes" }, { area: "review_policy" }, { area: "project_rule", label: "the billing code" },
    ];
    for (const a of all) {
      const w = areaWords(a)!;
      expect(w).toBeTruthy();
      expect(w).not.toMatch(/_|grant|host/);
    }
    expect(areaWords({ area: "sandboxing" })).toBe("code that limits what a program can reach");
    expect(areaWords({ area: "project_rule", label: "  " })).toBe("something the project’s review rules protect");
    expect(areaWords({ area: "reviewer", reason: "It changes how tokens are stored." })).toBeNull();
  });
  it("says why in one sentence, the reviewer's reason as well as the areas", () => {
    expect(approvalSentence(need([{ area: "updates" }]))).toBe("This change affects updates, so it needs your approval.");
    expect(approvalSentence(need([{ area: "credentials" }, { area: "credentials" }, { area: "installers" }])))
      .toBe("This change affects keys, tokens or passwords and installers, so it needs your approval.");
    expect(approvalSentence(need([{ area: "updates" }, { area: "reviewer", reason: "Risky." }])))
      .toBe("This change affects updates, so it needs your approval. The reviewer asked for it too.");
    expect(approvalSentence(need([{ area: "reviewer", reason: "Risky." }]))).toBe("The reviewer asked for your approval before it merges.");
    expect(approvalSentence(need([]))).toBe("This change needs your approval before it merges.");
  });
  it("says so plainly when Ophio couldn't tell what a change touches", () => {
    const reason = "Ophio couldn't list the files this pull request changes";
    expect(areaWords({ area: "unchecked", reason })).toBeNull();
    expect(approvalSentence(need([{ area: "unchecked", reason }]))).toBe("Ophio couldn’t check what this change touches, so it needs your approval.");
    expect(approvalSentence(need([{ area: "signing" }, { area: "unchecked", reason }])))
      .toBe("This change affects signing, so it needs your approval. Ophio couldn’t check everything it touches.");
    expect(uncheckedReasons(need([{ area: "unchecked", reason }]))).toEqual([`${reason}.`]);
  });
});

it("words each verdict as the Findings list and the details show it", () => {
  expect(verdictWords("confirmed", "reproduced")).toBe("Confirmed — shown by running it");
  expect(verdictWords("confirmed", "source_reasoning")).toBe("Confirmed — from reading the code");
  expect(verdictWords("confirmed", null)).toBe("Confirmed");
  expect(verdictWords("rejected", null, "intended")).toBe("Set aside — intended");
  expect(verdictWords("uncertain", null, " ")).toBe("Set aside");
  expect(verdictWords(null, null)).toBe("Not checked yet");
});

it("says which review rules were followed, and why none were", () => {
  expect(rulesWords(rules)).toEqual(["It followed the project’s review rules in these files: REVIEW.md and src/orders/REVIEW.md from commit 0a9b8c7 on origin/main."]);
  expect(rulesWords(rulesIgnored)).toEqual([
    "It followed the project’s review rules in REVIEW.md from commit 0a9b8c7 on origin/main.",
    "This change edits the review rules. Its edits were not followed; the rules already in the project were.",
  ]);
  expect(rulesWords(noRules)).toEqual(["No review rules were used. The project has no REVIEW.md."]);
  expect(rulesWords({ truncated: true, files: ["REVIEW.md"] })).toEqual([
    "It followed the project’s review rules in REVIEW.md.", "Some rule text was left out to keep within the size limit.",
  ]);
  expect(rulesWords({})).toEqual(["No review rules were used: the project has none."]);
  expect(rulesWords(null)).toEqual(["The computer didn’t record which review rules it used."]);
});

describe("a task's pull request", () => {
  const task = (github: Task["github"]): Task => ({ github } as Task);
  const pr = { number: 9, url: "", state: "open" as const, draft: false, head: commits.signing, passed: 2, failed: 0, pending: 0, unresolved: 0, conflict: false };
  it("reads nothing where the computer recorded no review and no approval", () => {
    expect(taskReviewInput(task({ link: { type: "new_pr" }, rounds: 0, pr }))).toBeNull();
    expect(taskReviewInput(task(undefined))).toBeNull();
    expect(prNeedLine(task({ link: { type: "new_pr" }, rounds: 0, pr }))).toBeNull();
  });
  it("gives Needs you a line for an approval or a stopped review, and nothing otherwise", () => {
    expect(prNeedLine(task({ link: { type: "new_pr" }, rounds: 1, pr, approval: signingApproval })))
      .toEqual({ kind: "approval", pill: "Approve", line: "It affects updates, signing and the review rules themselves, so it needs your approval to merge.", areas: signingApproval.areas });
    const raised = { areas: [{ area: "reviewer" as const, reason: "x" }], head: commits.signing };
    expect(prNeedLine(task({ link: { type: "new_pr" }, rounds: 1, pr, approval: raised })))
      .toEqual({ kind: "approval", pill: "Approve", line: "It needs your approval to merge.", areas: raised.areas });
    expect(prNeedLine(task({ link: { type: "new_pr" }, rounds: 3, pr, review: prReviews.escalated })))
      .toEqual({ kind: "escalated", pill: "Your call", line: "Its review stopped and needs you to decide what happens next." });
    // A stopped review that also needs approval names both, so merging from the row approves what it said.
    expect(prNeedLine(task({ link: { type: "new_pr" }, rounds: 3, pr, review: prReviews.escalated, approval: signingApproval })))
      .toEqual({
        kind: "escalated", pill: "Your call", areas: signingApproval.areas,
        line: "Its review stopped and needs you to decide what happens next. It affects updates, signing and the review rules themselves, so it needs your approval to merge.",
      });
    expect(prNeedLine(task({ link: { type: "new_pr" }, rounds: 1, pr, review: prReviews.clean }))).toBeNull();
    // Approval for an older commit waits for a fresh look, so it isn't asked for.
    expect(prNeedLine(task({ link: { type: "new_pr" }, rounds: 1, pr: { ...pr, head: HEAD }, approval: signingApproval }))).toBeNull();
    expect(prNeedLine(task({ link: { type: "new_pr" }, rounds: 1, pr: { ...pr, state: "merged" }, approval: signingApproval }))).toBeNull();
  });
  it("words the finder fixture's findings", () => {
    expect(finderReview.findings!.map((f) => verdictWords(f.verdict, f.evidence, f.reason))).toEqual([
      "Confirmed — shown by running it", "Set aside — Banker's rounding is intended and documented in money.rs.",
    ]);
  });
});
