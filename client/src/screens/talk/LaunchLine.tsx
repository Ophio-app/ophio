// SPDX-License-Identifier: Apache-2.0
//
// A task's permission setting in words. When it lets the agent act without
// asking, a warning says so every time; the command-line flags it starts
// with stay folded under "What it’s started with", as in Settings.

import { TriangleAlert } from "lucide-react";
import { launchFlags, launchRisk, launchSummary } from "../../app/launchPolicy";
import type { HarnessKind, LaunchPolicy, TaskMode } from "../../protocol/types";

export function LaunchLine({ harness, policy, mode = "work", className = "", label = true, risk = true }: {
  harness: HarnessKind;
  policy: LaunchPolicy | null | undefined;
  mode?: TaskMode;
  className?: string;
  /** Off where the setting's name is already shown, as on its chips. */
  label?: boolean;
  /** Off where the warning is said nearby, as by Review's Mode. */
  risk?: boolean;
}) {
  const warning = risk ? launchRisk(harness, policy, mode) : null;
  const flags = launchFlags(harness, policy, mode);
  return (
    <div className={`launch-line ${className}`}>
      {label && <p>{flags ? `Permission prompts: ${launchSummary(harness, policy, mode)}` : launchSummary(harness, policy, mode)}</p>}
      {warning && <p className="launch-line__risk" role="note"><TriangleAlert aria-hidden strokeWidth={2.2} /> {warning}</p>}
      {flags && <details className="launch-line__fold">
        <summary>What it’s started with</summary>
        <p>{flags}</p>
      </details>}
    </div>
  );
}
