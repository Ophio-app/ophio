// SPDX-License-Identifier: Apache-2.0
//
// One review result in plain sentences, written from what the computer
// recorded: what the review found and what became of it, whether the pull
// request's checks passed, what each confirmed problem rests on, and whether
// the change waits for the person's approval of its exact commit. Those are
// four separate facts and none stands in for another: a review that found
// nothing has not proved the change right, and passing checks are not a
// review. The phone only words them; it decides nothing.

import { useHost } from "./context";
import type {
  ApprovalNeed, EvidenceKind, FindingDisposition, FindingVerdict, PrReview, PrSummary, ReviewFinding, ReviewRulesUsed, SensitiveArea, Task,
} from "../protocol/types";

/** The computer gives review tasks the project's review rules, verifies findings and records each round of review. */
export const REVIEW_RULES_FEATURE = "review_rules_v1";
export const useReviewSupported = () => useHost((s) => !!s.capabilities?.features.includes(REVIEW_RULES_FEATURE));

/** Rounds of review a pull request gets before it waits for the person. */
export const MAX_ROUNDS = 3;

const NUMBERS = ["no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve"];

/** A count in words where that reads better: "two", then 13 and up as figures. */
export const numberWord = (n: number): string => NUMBERS[n] ?? String(n);
/** The first letter up, for the start of a sentence. */
export const cap = (s: string): string => s.charAt(0).toUpperCase() + s.slice(1);
/** "one problem", "two problems". */
export const counted = (n: number, one: string, many = `${one}s`): string => `${numberWord(n)} ${n === 1 ? one : many}`;
/** "a", "a and b", "a, b and c". */
export function listWords(items: string[]): string {
  if (items.length <= 1) return items[0] ?? "";
  return `${items.slice(0, -1).join(", ")} and ${items.at(-1)}`;
}
/** A commit as people see it on GitHub. */
export const shortCommit = (sha: string): string => sha.slice(0, 7);
const sentence = (s: string): string => { const t = s.trim(); return /[.!?]$/.test(t) ? t : `${t}.`; };

/** What each sensitive area is, as "This change affects …" ends. A reviewer's own reason isn't one of these. */
export function areaWords(area: SensitiveArea): string | null {
  switch (area.area) {
    case "permissions": return "permissions";
    case "pairing_or_login": return "pairing or signing in";
    case "credentials": return "keys, tokens or passwords";
    case "sandboxing": return "code that limits what a program can reach";
    case "updates": return "updates";
    case "signing": return "signing";
    case "release_or_build": return "how the app is built or released";
    case "installers": return "installers";
    case "data_changes": return "stored data in a way that can’t be undone";
    case "review_policy": return "the review rules themselves";
    case "project_rule": return area.label.trim() || "something the project’s review rules protect";
    case "reviewer": return null;
    case "unchecked": return null;
  }
}

/** The reasons a reviewer gave for wanting the person's approval, in its words. */
export const reviewerReasons = (need: ApprovalNeed): string[] =>
  need.areas.flatMap((a) => (a.area === "reviewer" && a.reason.trim() ? [a.reason.trim()] : []));

/** Why Ophio couldn't tell what a change touches, in the computer's words. */
export const uncheckedReasons = (need: ApprovalNeed): string[] =>
  need.areas.flatMap((a) => (a.area === "unchecked" && a.reason.trim() ? [sentence(a.reason)] : []));

/** The sentence that says why a change waits for the person's approval. */
export function approvalSentence(need: ApprovalNeed): string {
  const areas = [...new Set(need.areas.flatMap((a) => { const w = areaWords(a); return w ? [w] : []; }))];
  const reviewer = need.areas.some((a) => a.area === "reviewer");
  const unchecked = need.areas.some((a) => a.area === "unchecked");
  if (!areas.length) {
    if (unchecked) return `Ophio couldn’t check what this change touches, so it needs your approval.${reviewer ? " The reviewer asked for it too." : ""}`;
    return reviewer ? "The reviewer asked for your approval before it merges." : "This change needs your approval before it merges.";
  }
  const also = [reviewer ? " The reviewer asked for it too." : "", unchecked ? " Ophio couldn’t check everything it touches." : ""].join("");
  return `This change affects ${listWords(areas)}, so it needs your approval.${also}`;
}

export type VerdictLabel = { short: string; tone: "ok" | "warn" | "neutral" };

/**
 * A finding's verdict as the Findings list and the result's details say it:
 * "Confirmed — shown by running it", "Set aside — <reason>", "Not checked yet".
 */
export function verdictWords(verdict: FindingVerdict | null | undefined, evidence: EvidenceKind | null | undefined, reason?: string | null): string {
  const why = reason?.trim();
  switch (verdict) {
    case "confirmed":
      return evidence === "reproduced" ? "Confirmed — shown by running it" : evidence === "source_reasoning" ? "Confirmed — from reading the code" : "Confirmed";
    case "rejected":
    case "uncertain":
      return why ? `Set aside — ${why}` : "Set aside";
    default:
      return "Not checked yet";
  }
}

/** Why a set-aside finding wasn't acted on, as a short label beside its reason. */
export const SET_ASIDE_WORDS: Record<"rejected" | "uncertain", string> = { rejected: "Not a problem", uncertain: "Couldn’t be settled" };

/** What became of a confirmed finding, as a short label. */
export function dispositionWords(d: FindingDisposition): string {
  switch (d.state) {
    case "open": return "Still open";
    case "filed": return `Filed as issue #${d.number}`;
    case "fixing": return "Being fixed";
    case "corrected": return "Corrected";
    case "set_aside": return "Set aside";
  }
}

/** Whether a finding was set aside: judged not a problem or unsettled, or not acted on for another reason. */
export const setAside = (f: ReviewFinding): boolean =>
  f.verdict === "rejected" || f.verdict === "uncertain" || f.disposition.state === "set_aside";
/** Confirmed and acted on, or to be. */
export const confirmed = (f: ReviewFinding): boolean => f.verdict === "confirmed" && !setAside(f);
/** Not judged by the verifier yet. */
export const unchecked = (f: ReviewFinding): boolean => !f.verdict && !setAside(f);

/** Where a finding is, as `path:line`. */
export const findingWhere = (f: ReviewFinding): string | null => (f.path ? `${f.path}${f.line ? `:${f.line}` : ""}` : null);

/** The pull request's checks on GitHub, as its summary counts them. */
export type ChecksCount = Pick<PrSummary, "passed" | "failed" | "pending">;

export type FactTone = "ok" | "warn" | "stop" | "neutral" | "accent";
export interface Fact {
  /** One or two words, as a pill. */
  word: string;
  tone: FactTone;
  /** What it means, in a sentence or two. */
  text: string;
}

export interface ReviewResultInput {
  review: PrReview | null | undefined;
  approval: ApprovalNeed | null | undefined;
  /** The pull request's newest commit, as last read; null when unknown. */
  head: string | null;
  checks: ChecksCount | null;
  /** Merged, closed or still open. */
  state: PrSummary["state"];
}

export interface ReviewResult {
  /** The headline: plain sentences, the most important last so it reads as what to do. */
  headline: string[];
  review: Fact;
  checks: Fact;
  evidence: Fact;
  approval: Fact;
  /** Why it waits for the person, when it does: escalated, or an approval for its newest commit. */
  needsYou: "escalated" | "approval" | null;
  /** Approval is for this exact commit, the pull request's newest. */
  approvalHead: string | null;
  /** A commit newer than the last review is on the pull request and no review of it is under way. */
  unreviewed: boolean;
}

function groupBy(findings: ReviewFinding[]) {
  const by = (state: FindingDisposition["state"]) => findings.filter((f) => confirmed(f) && f.disposition.state === state);
  return { corrected: by("corrected"), fixing: by("fixing"), open: by("open"), filed: by("filed") };
}

/** "Two confirmed problems were corrected. One more is being fixed." */
function foundSentences(findings: ReviewFinding[]): string[] {
  const g = groupBy(findings);
  const parts: [number, (n: number, first: boolean) => string][] = [
    [g.corrected.length, (n, first) => `${first ? counted(n, "confirmed problem") : `${numberWord(n)} more`} ${n === 1 ? "was" : "were"} corrected`],
    [g.fixing.length, (n, first) => `${first ? counted(n, "confirmed problem") : `${numberWord(n)} more`} ${n === 1 ? "is" : "are"} being fixed`],
    [g.filed.length, (n, first) => {
      const numbers = g.filed.map((f) => `#${(f.disposition as { number: number }).number}`);
      return `${first ? counted(n, "confirmed problem") : `${numberWord(n)} more`} ${n === 1 ? `was filed as issue ${numbers[0]}` : `were filed as issues ${listWords(numbers)}`}`;
    }],
    [g.open.length, (n, first) => `${first ? counted(n, "confirmed problem") : `${numberWord(n)} more`} ${n === 1 ? "is" : "are"} still open`],
  ];
  let first = true;
  return parts.flatMap(([n, say]) => {
    if (!n) return [];
    const s = sentence(cap(say(n, first)));
    first = false;
    return [s];
  });
}

function checksFact(checks: ChecksCount | null): Fact {
  if (!checks) return { word: "Unknown", tone: "neutral", text: "Its checks on GitHub haven’t been read yet." };
  if (checks.failed) return { word: checks.failed === 1 ? "One failed" : `${cap(numberWord(checks.failed))} failed`, tone: "stop", text: `${cap(counted(checks.failed, "check"))} on GitHub ${checks.failed === 1 ? "has" : "have"} failed.` };
  if (checks.pending) return { word: "Running", tone: "accent", text: "Its checks on GitHub are still running." };
  if (checks.passed) return { word: "Passed", tone: "ok", text: checks.passed === 1 ? "Its one check on GitHub passed." : checks.passed === 2 ? "Both of its checks on GitHub passed." : `All ${numberWord(checks.passed)} of its checks on GitHub passed.` };
  return { word: "None", tone: "neutral", text: "It has no checks on GitHub." };
}

/** What the checks sentence in the headline says, with the corrections when there are some. */
function checksSentence(checks: ChecksCount | null, corrected: boolean): string | null {
  const lead = corrected ? "The corrections were reviewed" : null;
  if (!checks || (!checks.failed && !checks.pending && !checks.passed)) return lead && `${lead}.`;
  if (checks.failed) {
    const failed = `${counted(checks.failed, "check")} on the pull request ${checks.failed === 1 ? "has" : "have"} failed`;
    return lead ? `${lead}, but ${failed}.` : sentence(cap(failed));
  }
  if (checks.pending) return lead ? `${lead}. The pull request’s checks are still running.` : "The pull request’s checks are still running.";
  return lead ? `${lead} and the pull request’s checks passed.` : "The pull request’s checks passed.";
}

function evidenceFact(findings: ReviewFinding[], done: boolean): Fact {
  const sure = findings.filter(confirmed);
  const waiting = findings.filter(unchecked).length;
  const ran = sure.filter((f) => f.evidence === "reproduced").length;
  const read = sure.filter((f) => f.evidence === "source_reasoning").length;
  const other = sure.length - ran - read;
  if (!sure.length) {
    if (waiting) return { word: "Being checked", tone: "accent", text: `A second agent is checking ${waiting === 1 ? "the possible problem" : `the ${numberWord(waiting)} possible problems`} before anything is done about ${waiting === 1 ? "it" : "them"}.` };
    return done
      ? { word: "Nothing to show", tone: "neutral", text: "No problem was confirmed, so there was nothing to show. That doesn’t prove the change is right." }
      : { word: "Not yet", tone: "neutral", text: "Nothing has been found or checked yet." };
  }
  const lines: string[] = [];
  const all = (n: number) => (n === 2 ? "Both" : `All ${numberWord(n)}`);
  if (ran) lines.push(sure.length === ran ? (ran === 1 ? "The confirmed problem was shown by running it." : `${all(ran)} confirmed problems were shown by running them.`)
    : `${cap(numberWord(ran))} ${ran === 1 ? "was" : "were"} shown by running ${ran === 1 ? "it" : "them"}.`);
  if (read) lines.push(sure.length === read ? (read === 1 ? "The confirmed problem was found by reading the code. Nothing was run to show it." : `${all(read)} confirmed problems were found by reading the code. Nothing was run to show them.`)
    : `${cap(numberWord(read))} ${read === 1 ? "was" : "were"} found by reading the code, without running anything.`);
  if (other) lines.push(`${cap(numberWord(other))} ${other === 1 ? "doesn’t say" : "don’t say"} how ${other === 1 ? "it was" : "they were"} shown.`);
  if (waiting) lines.push(`${cap(counted(waiting, "more possible problem", "more possible problems"))} ${waiting === 1 ? "is" : "are"} still being checked.`);
  const word = ran === sure.length ? "Shown by running it" : read === sure.length ? "From reading the code" : "Mixed";
  return { word, tone: ran === sure.length ? "ok" : "neutral", text: lines.join(" ") };
}

/**
 * A pull request's review result, from its review, approval need, newest
 * commit and checks. Kept apart from any screen so every combination can be
 * read in a test.
 */
export function reviewResult({ review, approval, head, checks, state }: ReviewResultInput): ReviewResult {
  const findings = review?.findings ?? [];
  const reviewed = review?.reviewed_head ?? null;
  const reviewing = review?.reviewing_head && review.reviewing_head !== reviewed ? review.reviewing_head : null;
  const done = !!reviewed;
  const open = state === "open";
  const unreviewed = done && open && !!head && head !== reviewed && !reviewing;
  const rounds = review?.round ?? 0;
  const escalated = review?.escalated?.trim() || null;
  const sure = findings.filter(confirmed);
  const asideCount = findings.filter(setAside).length;
  const waiting = findings.filter(unchecked).length;
  const corrected = sure.some((f) => f.disposition.state === "corrected");
  const notRun = (review?.not_run ?? []).map((s) => s.trim().replace(/[.]+$/, "")).filter(Boolean);
  const approvalCurrent = !!approval && open && (!head || approval.head === head);

  const headline: string[] = [];
  if (!done) {
    headline.push(reviewing ? "A review of this change is under way." : "No review of this change has been recorded.");
  } else {
    if (!sure.length && !waiting) {
      headline.push(asideCount
        ? `The review found no confirmed problems. A second agent checked ${counted(asideCount, "possible problem")} and set ${asideCount === 1 ? "it" : "them"} aside.`
        : "The review found no problems.");
    } else {
      headline.push(...foundSentences(findings));
      if (waiting) headline.push(`A second agent is still checking ${counted(waiting, "possible problem")}.`);
    }
  }
  const checking = open || state === "merged" ? checksSentence(checks, done && corrected) : null;
  if (checking) headline.push(checking);
  if (notRun.length) headline.push(notRun.length === 1 ? `One check was not run: ${notRun[0]}.` : `${cap(numberWord(notRun.length))} checks were not run: ${listWords(notRun)}.`);
  if (done && reviewing) headline.push("A review of the newest commit is under way.");
  if (unreviewed) headline.push("Newer commits haven’t been reviewed yet.");
  if (escalated && open) headline.push("The review stopped and needs you to decide what happens next.");
  if (approval && open) headline.push(approvalCurrent ? approvalSentence(approval) : "An older commit needed your approval. Whether the newest one does is checked when you merge it.");

  const reviewFact: Fact = escalated && open
    ? { word: "Stopped", tone: "warn", text: `It stopped after ${counted(rounds || 1, "round")} and is waiting for you.${done ? ` The last one covered commit ${shortCommit(reviewed!)}.` : ""}` }
    : reviewing
      ? { word: "Under way", tone: "accent", text: `Reviewing commit ${shortCommit(reviewing)}${rounds >= 1 ? `, after ${counted(rounds, "round")} so far` : ""}.` }
      : done
        ? { word: "Done", tone: "ok", text: `Reviewed up to commit ${shortCommit(reviewed!)}. That took ${counted(rounds || 1, "round")} of at most ${numberWord(MAX_ROUNDS)}.${unreviewed ? " Newer commits haven’t been reviewed." : ""}` }
        : { word: "Not yet", tone: "neutral", text: "No review of it has been recorded." };

  const approvalFact: Fact = !open
    ? { word: state === "merged" ? "Merged" : "Closed", tone: "neutral", text: state === "merged" ? "It has merged." : "It was closed without merging." }
    : approval
      ? approvalCurrent
        ? { word: "Needs yours", tone: "warn", text: `${approvalSentence(approval)} Approving covers commit ${shortCommit(approval.head)} only.` }
        : { word: "Out of date", tone: "neutral", text: `Commit ${shortCommit(approval.head)} needed your approval. The newest commit is checked when you merge it.` }
      : { word: "Not needed", tone: "neutral", text: "Nothing it changes needs your approval. You still choose when to merge it." };

  return {
    headline,
    review: reviewFact,
    checks: open || state === "merged" ? checksFact(checks) : { word: "Closed", tone: "neutral", text: "It was closed, so its checks no longer matter." },
    evidence: evidenceFact(findings, done),
    approval: approvalFact,
    needsYou: !open ? null : escalated ? "escalated" : approvalCurrent ? "approval" : null,
    approvalHead: approvalCurrent ? approval!.head : null,
    unreviewed,
  };
}

/** A task's pull request review, if the computer recorded one or a need for approval. */
export function taskReviewInput(task: Task, pr?: PrSummary | null): ReviewResultInput | null {
  const github = task.github;
  if (!github?.review && !github?.approval) return null;
  const summary = pr ?? github.pr ?? null;
  return {
    review: github.review ?? null,
    approval: github.approval ?? null,
    head: summary?.head ?? null,
    checks: summary ? { passed: summary.passed, failed: summary.failed, pending: summary.pending } : null,
    state: summary?.state ?? "open",
  };
}

/** Why a pull request's task waits for the person, in a line for Needs you, or null. */
export function prNeedLine(task: Task | null | undefined, pr?: PrSummary | null): { kind: "escalated" | "approval"; pill: string; line: string; areas?: SensitiveArea[] } | null {
  const input = task ? taskReviewInput(task, pr) : null;
  if (!input) return null;
  const r = reviewResult(input);
  // An approval of the commit shown is named on the line whatever else it says, so merging from it approves what was read.
  const approval = r.approvalHead ? input.approval! : null;
  const words = approval ? [...new Set(approval.areas.flatMap((a) => { const w = areaWords(a); return w ? [w] : []; }))] : [];
  const asks = words.length ? `It affects ${listWords(words)}, so it needs your approval to merge.` : "It needs your approval to merge.";
  const areas = approval ? { areas: approval.areas } : {};
  if (r.needsYou === "escalated") {
    const stopped = "Its review stopped and needs you to decide what happens next.";
    return { kind: "escalated", pill: "Your call", line: approval ? `${stopped} ${asks}` : stopped, ...areas };
  }
  if (r.needsYou === "approval") return { kind: "approval", pill: "Approve", line: asks, ...areas };
  return null;
}

/** The rules a review followed, in sentences for its details. */
export function rulesWords(rules: ReviewRulesUsed | null | undefined): string[] {
  if (!rules) return ["The computer didn’t record which review rules it used."];
  const out: string[] = [];
  const from = rules.revision ? ` from commit ${shortCommit(rules.revision)}${rules.source ? ` on ${rules.source}` : ""}` : rules.source ? ` from ${rules.source}` : "";
  const files = rules.files ?? [];
  if (files.length) out.push(`It followed ${files.length === 1 ? "the project’s review rules in" : "the project’s review rules in these files:"} ${listWords(files)}${from}.`);
  else out.push(rules.missing?.trim() ? `No review rules were used. ${sentence(cap(rules.missing.trim()))}` : "No review rules were used: the project has none.");
  if (rules.ignored_changes) out.push("This change edits the review rules. Its edits were not followed; the rules already in the project were.");
  if (rules.truncated) out.push("Some rule text was left out to keep within the size limit.");
  return out;
}
