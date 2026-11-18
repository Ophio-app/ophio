// SPDX-License-Identifier: Apache-2.0
//
// How a task started from a chat's card stands, in the words its row in
// Tasks uses, so the card and the list never disagree: waiting, running,
// needs you, pull request open, merged, failed. It changes in place, and
// nothing about it alerts.

import type { Task } from "../../protocol/types";
import type { HostState } from "../../state/projection";
import type { Tone } from "../../design/Chip";
import { latestRun } from "../work/logic";
import { taskAttentionReason } from "../work/taskActivity";
import { taskRowBadge } from "./TaskBar";

export interface CardLive {
  label: string;
  tone: Tone;
  /** What it waits on the person for, when it does. */
  why: string | null;
}

export function cardLive(s: HostState, task: Task): CardLive {
  const run = latestRun(task, s.runs);
  const why = taskAttentionReason(s, task);
  const badge = taskRowBadge(task, run, s.queues[task.project_id], why);
  // What needs the person, what is going and what went wrong come first;
  // between turns, its pull request says how far the work has come.
  if (why || badge.tone === "accent" || badge.tone === "stop" || badge.tone === "warn") return { ...badge, why };
  const pr = task.github?.pr;
  if (pr?.state === "merged") return { label: "Merged", tone: "ok", why: null };
  if (pr?.state === "open") return { label: pr.draft ? "Draft pull request" : "Pull request open", tone: "accent", why: null };
  return { ...badge, why: null };
}
