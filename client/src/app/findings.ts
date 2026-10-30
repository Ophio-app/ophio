// SPDX-License-Identifier: Apache-2.0
//
// Find & fix without GitHub. A reviewer writes what it finds in the project's
// FINDINGS.md, one heading with a box per finding. Where the computer checks
// findings, a second agent then tries to disprove each one and writes its
// Verdict line under it. A fixer works through what wasn't set aside,
// ticking each box and saying what it did, then asks before deleting the
// file. The phone writes what each agent is asked and reads the file back
// through the computer; nothing here writes the file.

import { useHost } from "./context";
import { FOCUS_HINT, FOCUS_ISSUES, FOCUS_WORDS } from "./github";
import type { FindFocus, Task, TaskState } from "../protocol/types";
import { projectTasks } from "../state/projection";
import type { Finding } from "./findingsFile";

export type { Finding, Severity } from "./findingsFile";
export { isSetAside } from "./findingsFile";

export const FINDINGS_FILE = "FINDINGS.md";
export const FINDINGS_FEATURE = "findings_v1";

export type FindWhere = { scope: "project" } | { scope: "folder"; path: string } | { scope: "changes" };

const whereWords = (where: FindWhere) =>
  where.scope === "folder" ? `the folder ${where.path}`
    : where.scope === "changes" ? "what the current branch changes compared with the default branch, or the last 20 commits if it is the default branch"
      : "the whole project";

const FORMAT = [
  "## [ ] 1. A short title",
  "Severity: high, medium or low",
  "Where: path/to/file:line",
  "What is wrong: the concrete steps or inputs that make it happen, what should happen, and what happens instead.",
].join("\n");

const TICKING = 'Then tick its box ("## [x]") and add a line under it: "Fixed: what you did", or "Not a problem: why" if it is not real. If you can\'t fix it, leave the box empty and add "Not fixed: why".';

const KEEP_OUT = "Change nothing else in FINDINGS.md, and leave it out of any commit: it is a working list, not part of the project. Never change or remove a line that starts with \"Verdict:\".";

/** Fix all leaves alone what the second agent set aside. */
const SKIP_SET_ASIDE = 'Skip any finding whose "Verdict:" line says rejected or uncertain: a second agent checked it and set it aside. Leave its box empty and add nothing under it.';

/** Fix all after a checked review acts only on what the second agent confirmed. */
const ONLY_CONFIRMED = 'Fix only findings whose last "Verdict:" line says confirmed: a second agent checked those. Skip every other one, with no "Verdict:" line or one that says rejected or uncertain, and leave it for me: leave its box empty and add nothing under it.';

const ASK_TO_DELETE = "ask me whether to delete FINDINGS.md, and delete it only if I say yes.";

/**
 * What a reviewer is asked: to review and write what it finds in
 * FINDINGS.md, changing nothing else. `checked` is a computer that has a
 * second agent check each finding afterwards, which the reviewer is told.
 */
export function reviewPrompt(r: { focus: FindFocus | null; where: FindWhere; most: number; checked?: boolean }): string {
  const who = r.focus ? `a ${FOCUS_WORDS[r.focus].toLowerCase()}` : "a careful senior engineer";
  return [
    `Review ${whereWords(r.where)} as ${who} and write what you find to FINDINGS.md at the project's root. Change no other file, and don't commit.`,
    `Look for: ${FOCUS_HINT[r.focus ?? "default"]}`,
    `Read the code before judging it, and report only problems you can point to in it, not style preferences or guesses. For each, give its concrete failure: the steps or inputs that make it happen, what should happen, and what happens instead. Put the most serious first and write at most ${r.most}.`,
    ...(r.checked ? ["A second agent will check each finding before anything is fixed, and sets aside any it can't show is real. Don't write \"Verdict:\" lines yourself: only that agent writes them."] : []),
    `Write each finding as:\n\n${FORMAT}`,
    "If FINDINGS.md already exists, keep everything in it, skip anything it already lists, and number yours after its last. Leave every box empty for the fixer. When you're done, say how many you wrote.",
  ].join("\n\n");
}

/**
 * What a fixer is asked to do with every finding still open. `checked`
 * is a fix queued after a review the computer checks: it fixes only what
 * the second agent confirmed, never a finding it didn't check.
 */
export function fixAllPrompt(r: { checked?: boolean } = {}): string {
  return [
    "Work through FINDINGS.md at the project's root, one unticked finding at a time, in order. For each, check it is real, fix it, and test the fix the way this project tests things.",
    r.checked ? ONLY_CONFIRMED : SKIP_SET_ASIDE,
    TICKING,
    KEEP_OUT,
    "If there is no FINDINGS.md, or nothing in it is left to fix, say so and stop.",
    `When every finding you were to fix is ticked, ${ASK_TO_DELETE}`,
  ].join("\n\n");
}

/** What a fixer is asked to do with one finding. */
export function fixOnePrompt(f: Pick<Finding, "number" | "title">): string {
  return [
    `Fix finding ${f.number} in FINDINGS.md at the project's root, "${f.title}". Check it is real, fix it, and test the fix the way this project tests things.`,
    "If it has a \"Verdict:\" line, a second agent checked it: read what it says. I chose this finding myself, so fix it even if that agent set it aside, unless you find it isn't real.",
    TICKING,
    KEEP_OUT,
    `If no unticked finding is left except ones a second agent set aside, ${ASK_TO_DELETE}`,
  ].join("\n\n");
}

/** A task's title for each, as Work lists it. */
export function reviewTitle(focus: FindFocus | null, where: FindWhere): string {
  const what = focus ? `${FOCUS_ISSUES[focus]} issues` : "issues";
  return `Find ${what}${where.scope === "folder" ? ` in ${where.path}` : where.scope === "changes" ? " in recent changes" : ""} for ${FINDINGS_FILE}`;
}
export const fixAllTitle = () => `Fix what's in ${FINDINGS_FILE}`;
export const fixOneTitle = (f: Pick<Finding, "number" | "title">) => `Fix finding ${f.number}: ${f.title}`.slice(0, 120);

/** Whether the computer reads a project's FINDINGS.md for the phone. */
export const useFindingsSupported = () => useHost((s) => !!s.capabilities?.features.includes(FINDINGS_FEATURE));

/** Task states in which a checker hasn't finished: waiting to start, working, or asking something. */
const UNFINISHED: TaskState[] = ["accepted", "dispatching", "working", "awaiting_decision"];
/** Task states in which a checker ended without finishing. */
const STOPPED: TaskState[] = ["failed", "interrupted", "reconciliation_required"];

/**
 * How the check of the project's latest review without GitHub stands: a
 * second agent still checking its findings, one that stopped before it
 * finished, or null when there is none or it is done.
 */
export function useFindingsCheck(projectId: string): "checking" | "stopped" | null {
  return useHost((s) => {
    let review: Task | null = null;
    for (const t of projectTasks(s, projectId)) {
      if (t.review?.role === "find" && !t.github && (!review || t.created_at > review.created_at)) review = t;
    }
    const id = review?.review?.verified_by;
    const checker = id ? s.tasks[id] : undefined;
    if (!checker) return null;
    return UNFINISHED.includes(checker.state) ? "checking" : STOPPED.includes(checker.state) ? "stopped" : null;
  });
}
