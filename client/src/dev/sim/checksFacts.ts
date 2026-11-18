// SPDX-License-Identifier: Apache-2.0
import type { Check } from "../../protocol/generated/Check";
import type { CheckFacts } from "../../protocol/generated/CheckFacts";

function duration(seconds: number): string {
  const minutes = Math.floor(seconds / 60), rest = seconds % 60;
  return [minutes ? `${minutes} minute${minutes === 1 ? "" : "s"}` : "", rest || !minutes ? `${rest} second${rest === 1 ? "" : "s"}` : ""].filter(Boolean).join(" ");
}

/** The simulated computer computes these words, just as the real store does.
 * Names, descriptions and proposal notes never supply factual claims. */
export function checkFacts(check: Check): CheckFacts {
  return {
    when: check.when === "before_merge" ? "Before merging (not run automatically yet)"
      : !check.paths.length ? "After every task that changes files"
      : check.paths.every(path => /^(src|test|tests)\//.test(path)) ? "After a task changes the app’s code or tests"
      : "After a task changes the watched files or folders",
    internet: check.network ? "Uses the internet" : "Does not use the internet",
    time_limit: `Time limit: ${duration(check.timeout_secs)}`,
    typical_time: check.typical_secs === null ? null : `Takes about ${duration(check.typical_secs)}`,
  };
}

export function checkDifferences(before: Check | undefined, after: Check): string[] {
  if (!before) return ["Adds a new check"];
  const changes: string[] = [];
  if (before.name !== after.name) changes.push("Changes the name");
  if (before.about !== after.about) changes.push("Changes what it checks");
  if (before.run !== after.run) changes.push("Now runs a different command");
  if (before.setup !== after.setup) changes.push("Changes its setup (not run automatically yet)");
  if (JSON.stringify(before.paths) !== JSON.stringify(after.paths)) changes.push(!after.paths.length ? "Now runs after every task that changes files" : "Changes which files or folders make it run");
  if (before.when !== after.when) changes.push(after.when === "every_task" ? "Now runs after tasks" : "Now runs before merging (not run automatically yet)");
  if (before.network !== after.network) changes.push(after.network ? "Now uses the internet" : "No longer uses the internet");
  if (before.timeout_secs !== after.timeout_secs) changes.push(`Time limit changes from ${duration(before.timeout_secs)} to ${duration(after.timeout_secs)}`);
  if (JSON.stringify(before.inputs) !== JSON.stringify(after.inputs)) changes.push("Changes the files it depends on");
  if (before.enabled !== after.enabled) changes.push(after.enabled ? "Turns this check on" : "Turns this check off");
  return changes;
}
