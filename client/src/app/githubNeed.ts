// SPDX-License-Identifier: Apache-2.0
//
// Why an issue or an agent's pull request is under Needs you, in words for
// someone who has never used the app: what happened, what the agent said in
// its own words, and the one thing to do next. The row shows the headline
// and a short pill; the sheet shows all of it, with the buttons that do the
// next thing. A pull request is there when its review stopped for the
// person, or when it needs their approval of its newest commit.

import { useMemo } from "react";
import type { AttentionItem, GithubItem, SensitiveArea, Task } from "../protocol/types";
import { prNeedLine } from "./review";
import type { HostState } from "../state/projection";
import { useHost } from "./context";

/** Labels Øphio puts on issues for its own bookkeeping, not for people. */
export const OWN_LABELS = ["agent", "agent-found", "agent-not-reproduced"];
const CHECKED_LABEL = "agent-not-reproduced";

export type NeedKind =
  /** The fixer found the code already fixes it. */
  | "fixed"
  /** The fixer couldn't make it happen. */
  | "not_reproduced"
  /** A fixer said one of those before, and its task is gone. */
  | "checked_before"
  /** The fixer's pull request ran out of fix rounds or time. */
  | "stuck"
  /** The agent asked something. */
  | "question"
  /** The agent's run failed or needs a check. */
  | "failed"
  /** The agent raised something else. */
  | "notice"
  /** A Sort said it needs a person. */
  | "sorted"
  /** The person put it here themselves. */
  | "held"
  /** A pull request's review stopped and waits for the person. */
  | "review_stopped"
  /** A pull request touches something sensitive: it merges only once the person approves its newest commit. */
  | "approve";

export interface Need {
  kind: NeedKind;
  /** The row's pill: the next step, in a word or two. */
  pill: string;
  /** What happened, in a sentence: the sheet's heading, and the row's line unless `row` says it better. */
  headline: string;
  /** The row's line where the reason itself says more than the headline: the Sort's, or the person's own. */
  row?: string;
  /** Who said `said`: "What the agent found", "The Sort asks". */
  saidBy?: string;
  /** What the agent or the Sort said, in its own words. */
  said?: string;
  /** What the person does next. */
  ask: string;
}

/**
 * What the agent last said about `task` that is still open: the newest
 * unresolved alert's text, else its title.
 */
export function openAlert(s: HostState, task: Task | null): AttentionItem | null {
  if (!task) return null;
  let newest: AttentionItem | null = null;
  for (const a of Object.values(s.attention)) {
    if (a.task_id !== task.task_id || a.resolved_at != null || a.kind === "completed") continue;
    if (!newest || a.created_at > newest.created_at) newest = a;
  }
  return newest;
}

/**
 * Why `item` waits for the person, or null when it doesn't. `task` is the
 * one working it, as this device has it, and `alert` what it last raised.
 */
export function needOf(item: GithubItem, task: Task | null, alert: AttentionItem | null): Need | null {
  const github = task?.github;
  const raised = alert?.body?.trim() || alert?.title || undefined;
  if (task && github?.stage === "not_reproduced") {
    const said = github.finding ?? raised;
    return github.already_fixed
      ? {
        kind: "fixed", pill: "Close it?", headline: "The agent found this already fixed",
        saidBy: "What the agent found", said,
        ask: "If that’s right, close the issue and you’re done. If it still happens for you, tap It still happens and say how; the agent tries again.",
      }
      : {
        kind: "not_reproduced", pill: "Close it?", headline: "The agent couldn’t make this happen",
        saidBy: "What the agent found", said,
        ask: "If it’s no longer a problem, close the issue and you’re done. If it still happens for you, tap It still happens and say how to make it happen; the agent tries again.",
      };
  }
  if (task && github?.stage === "capped") {
    const pr = github.pr ? `pull request #${github.pr.number}` : "pull request";
    return {
      kind: "stuck", pill: "Stuck", headline: `The agent’s ${pr} is stuck`,
      saidBy: "Why it stopped", said: raised,
      ask: "The agent stopped trying to get it to pass. Open its task to see what’s wrong, then merge it, close it, or tell the agent what to change.",
    };
  }
  if (task && (item.status === "waiting_for_you" || task.attention_count > 0)) {
    if (task.state === "awaiting_decision") {
      return {
        kind: "question", pill: "Answer", headline: "The agent asked you something",
        saidBy: "It asked", said: raised ?? task.status_line ?? undefined,
        ask: "Open its task and answer. It carries on once you do.",
      };
    }
    if (task.state === "failed" || task.state === "reconciliation_required") {
      return {
        kind: "failed", pill: "Failed", headline: "The agent’s work on this failed",
        saidBy: "What it said", said: raised ?? task.status_line ?? undefined,
        ask: "Open its task to see what went wrong, or tap Try again to start a new fix.",
      };
    }
  }
  if (item.status === "waiting_for_you") {
    return {
      kind: "notice", pill: "Look", headline: alert?.title ?? "The agent needs you to look at its task",
      ...(alert?.body ? { saidBy: "It said", said: alert.body } : {}),
      ask: "Open its task to see what it needs.",
    };
  }
  if (item.group !== "needs_you" || (task && item.status)) return null;
  if (!task && item.labels.includes(CHECKED_LABEL) && !item.grouped_by_you) {
    return {
      kind: "checked_before", pill: "Close it?", headline: "An agent already checked this",
      ask: "It couldn’t make it happen, or found it already fixed; its comment on GitHub says which. If that’s right, close the issue. If it still happens, tap It still happens and say how.",
    };
  }
  if (item.grouped_by_you) {
    return {
      kind: "held", pill: "On hold", headline: "You asked this to wait for you", row: item.triage_note ?? undefined,
      ask: "When you’re ready, tap Start to have the agent fix it, or move it to Agent can take it so Run all does. Close it if it’s no longer needed.",
    };
  }
  return {
    kind: "sorted", pill: "Your call", headline: "An agent can’t start this until you answer", row: item.triage_note ?? undefined,
    saidBy: "The Sort asks", said: item.triage_note,
    ask: "Write your answer below and tap Start; the agent works from it. If it isn’t needed, close the issue.",
  };
}

/**
 * Why an agent's pull request waits for the person, or null when it
 * doesn't: its review stopped, or it needs their approval of the newest
 * commit, `item.head` as the list read it.
 */
export function prNeedOf(item: GithubItem, task: Task | null): Need | null {
  const pr = task?.github?.pr;
  if (!item.pr || !task || !pr || pr.number !== item.number) return null;
  const line = prNeedLine(task, item.head ? { ...pr, head: item.head } : pr);
  if (!line) return null;
  const escalated = task.github?.review?.escalated?.trim();
  return line.kind === "approval"
    ? {
      kind: "approve", pill: line.pill, headline: "This change needs your approval", row: line.line,
      ask: "Open it to see what it affects and what the review found. Merging it from here is your approval of its newest commit, and only that commit.",
    }
    : {
      kind: "review_stopped", pill: line.pill, headline: "Its review stopped and needs you", row: line.line,
      ...(escalated ? { saidBy: "Why it stopped", said: escalated } : {}),
      ask: "Open it to read what the review found. Then merge it, close it, or tell the agent what to change.",
    };
}

const NONE: SensitiveArea[] = [];

/**
 * What an agent's pull request's row showed as needing the person's approval, for a merge from the list to
 * approve only that: its areas, or none. A row whose review stopped names the approval too.
 */
export function approvalShownOf(item: GithubItem, task: Task | null): SensitiveArea[] {
  const pr = task?.github?.pr;
  if (!item.pr || !task || !pr || pr.number !== item.number) return NONE;
  return prNeedLine(task, item.head ? { ...pr, head: item.head } : pr)?.areas ?? NONE;
}

/** Kinds whose next step isn't a new fix: the sheet leads with their buttons and keeps the start form folded away. */
export const FOLDED: NeedKind[] = ["fixed", "not_reproduced", "checked_before", "stuck", "question", "failed", "notice"];

/** Kinds the person settles by closing the issue. */
export const CLOSES: NeedKind[] = ["fixed", "not_reproduced", "checked_before"];

/** Why an issue or an agent's pull request waits for the person, as this device has its task. */
export function useNeed(item: GithubItem): Need | null {
  const task = useHost((s) => (item.task_id ? s.tasks[item.task_id] ?? null : null));
  const alert = useHost((s) => (item.pr ? null : openAlert(s, task)));
  return useMemo(() => (item.pr ? prNeedOf(item, task) : needOf(item, task, alert)), [item, task, alert]);
}

/** An issue's labels, less Øphio's own. */
export const shownLabels = (labels: string[]): string[] => labels.filter((l) => !OWN_LABELS.includes(l));
