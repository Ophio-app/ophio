// SPDX-License-Identifier: Apache-2.0
//
// The Git branch a new task's work is meant for, or a new one for the agent
// to make. The agent is told it on every run; Ophio never creates or checks a
// branch out, so the folder's uncommitted changes are only ever touched by
// the agent, which is asked to keep them.

import type { TaskBranch } from "../protocol/types";

export const TASK_BRANCH_FEATURE = "task_branch_v1";
export const TASK_BRANCH_UPDATE = "Update Øphio on this computer to choose a branch. Nothing will be sent naming a branch it cannot pass on.";

export const changesText = (n: number) => `${n} uncommitted ${n === 1 ? "change" : "changes"}`;

/** The wire form: `new` is left off for an existing branch. */
export const branchCommand = (b: TaskBranch): TaskBranch => (b.new ? { name: b.name, new: true } : { name: b.name });

/**
 * What the folder has checked out, and what choosing `chosen` means for it.
 * `current` null with `known` true is a checked-out commit rather than a
 * branch. `exists` is a new branch's name already being on the computer.
 */
export function branchStatus(chosen: TaskBranch | null, current: string | null, dirty: number, known: boolean, exists = false): string | null {
  const changes = dirty ? ` · ${changesText(dirty)}` : "";
  const here = current ? `${current} is checked out` : "No branch is checked out";
  if (chosen?.new) {
    if (exists) return `${chosen.name} already exists. Choose it from the list, or the agent will ask you before using it.`;
    const from = known ? ` from ${current ?? "the checked-out commit"}${dirty ? `, bringing ${changesText(dirty)}` : ""}` : "";
    return `New branch${from}. The agent makes it before changing any file.`;
  }
  if (!known) return null;
  if (chosen === null) return `${here}${changes}.`;
  if (chosen.name === current) return `Checked out now${changes}.`;
  return dirty
    ? `${here}${changes}. The agent is asked to keep them if it switches.`
    : `${here}. The agent is asked to switch to ${chosen.name}.`;
}

/** As typed on a phone: spaces become hyphens, since Git allows none. */
export const branchNameInput = (typed: string) => typed.replace(/\s+/g, "-");

const CONTROL = /\p{Cc}/u;

/**
 * Why Git wouldn't accept `name` as a branch (`git check-ref-format
 * --branch`), or null. The companion applies the same rules.
 */
export function branchNameProblem(name: string): string | null {
  if (name === "") return "Enter a name for the branch.";
  if (new TextEncoder().encode(name).length > 255) return "Use a shorter name.";
  if (name === "@") return "“@” on its own isn’t a branch name.";
  if (name === "HEAD") return "“HEAD” is Git’s name for what’s checked out.";
  if (name.startsWith("-")) return "A branch name can’t start with “-”.";
  if (name.endsWith(".")) return "A branch name can’t end with “.”.";
  if (name.includes("..")) return "A branch name can’t contain “..”.";
  if (name.includes("@{")) return "A branch name can’t contain “@{”.";
  const bad = [...name].find((c) => CONTROL.test(c) || " ~^:?*[\\".includes(c));
  if (bad === " ") return "A branch name can’t contain spaces.";
  if (bad !== undefined) return CONTROL.test(bad) ? "A branch name can’t contain control characters." : `A branch name can’t contain “${bad}”.`;
  const parts = name.split("/");
  if (parts.some((p) => p === "")) return "A branch name can’t start or end with “/”, or have “//”.";
  if (parts.some((p) => p.startsWith("."))) return "No part of a branch name can start with “.”.";
  if (parts.some((p) => p.endsWith(".lock"))) return "No part of a branch name can end with “.lock”.";
  return null;
}
