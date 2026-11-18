// SPDX-License-Identifier: Apache-2.0
import type { GithubAction, Grants } from "../../protocol/types";

// This is the simulator's actual enforcement, not a parallel inventory. A new
// action must choose its grants here before the client can typecheck.
export const githubActionGrants = {
  set_settings: ["admin"],
  answer_warning: ["admin"],
  protect_branch: ["admin"],
  mark_pr_ready: ["tasks"],
  file_issue: ["tasks"],
  close_issue: ["tasks"],
  run_issues: ["tasks"],
  set_group: ["tasks"],
  stop_together: ["tasks"],
  set_when_green: ["tasks"],
  merge_pr: ["tasks", "approvals"],
  merge_together: ["tasks", "approvals"],
} as const satisfies Record<GithubAction["action"], readonly (keyof Grants)[]>;

export function githubActionAllowed(grants: Grants, action: GithubAction): boolean {
  const required = githubActionGrants[action.action];
  // Unknown wire input fails closed, even outside TypeScript callers.
  if (!required || !required.every((grant) => grants[grant])) return false;
  return action.action !== "set_when_green" || action.when_green !== "merge" || grants.approvals;
}
