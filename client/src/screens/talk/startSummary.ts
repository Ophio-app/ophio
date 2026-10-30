// SPDX-License-Identifier: Apache-2.0
//
// The line right above Review's Start for new work: where it will be, which
// agent does it, what it may do and when it starts. Each is chosen further up
// the review, often below the fold; this says all four where the button is,
// and in the warning tone when the agent will act without asking.

import { launchChoices, launchRisk } from "../../app/launchPolicy";
import type { HarnessKind, LaunchPolicy, QueueAuthorization, TaskMode } from "../../protocol/types";
import { modeLabel } from "./ModePicker";

export interface StartSummary {
  text: string;
  /** Said when the agent will act without asking, as everywhere else. */
  risk: string | null;
}

export function startSummary({ project, alone, agent, harness, policy, mode, placement, authorization, paused }: {
  project: string;
  /** A new task in a folder of its own. */
  alone: boolean;
  agent: string;
  harness: HarnessKind;
  policy: LaunchPolicy | null | undefined;
  mode: TaskMode;
  placement: "start_now" | "queue";
  authorization: QueueAuthorization;
  /** Next up is paused in the project. */
  paused: boolean;
}): StartSummary {
  const where = alone ? "On its own" : `In ${project}`;
  const label = policy ? launchChoices(harness).find((c) => c.value === policy)?.label : undefined;
  const access = mode !== "work" ? `${modeLabel(mode)}, read-only`
    : label ? `Work, “${label}”` : "Work, the agent’s own permission settings";
  // Adding one that starts by itself resumes a paused Next up.
  const when = placement === "start_now" ? "Starts now"
    : authorization === "auto_start" ? "Next up, starts by itself"
      : paused ? "Next up, paused" : "Next up, asks you first";
  return { text: [where, agent, access, when].join(" · "), risk: launchRisk(harness, policy, mode) };
}
