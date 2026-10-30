// SPDX-License-Identifier: Apache-2.0
//
// Said right above a button that starts work in another mode than the task
// it came from, so what a Plan or Ask proposes never becomes Work unnoticed.
// Agents that only read can't start anything themselves; the person's Start
// is what switches, and this is where they see it. Marked in the app's lime.

import { launchChoices } from "../../app/launchPolicy";
import type { AgentChoice } from "../../app/ui";
import type { TaskMode } from "../../protocol/types";
import { modeLabel } from "./ModePicker";

/** The launch flags work starts with, as the agent sheet names them. */
export function launchName(a: Pick<AgentChoice, "harness" | "launchPolicy">): string {
  const choice = a.launchPolicy ? launchChoices(a.harness).find(c => c.value === a.launchPolicy) : undefined;
  return choice ? `“${choice.label}”` : "the agent’s own permission settings";
}

export function ModeSwitch({ from, to, who = "This new task", flags }: {
  from: TaskMode;
  to: TaskMode;
  /** What switches, as the sentence's subject. */
  who?: string;
  /** What Work starts with, where it is known. */
  flags?: Pick<AgentChoice, "harness" | "launchPolicy"> | null;
}) {
  if (from === to) return null;
  return (
    <p className="mode-switch" role="note">
      <span className="mode-switch__what">Switching from {modeLabel(from)} to {modeLabel(to)}.</span>{" "}
      {to === "work" ? `${who} can change files${flags ? `, and starts with ${launchName(flags)}` : ""}.` : `${who} only reads.`}
    </p>
  );
}
