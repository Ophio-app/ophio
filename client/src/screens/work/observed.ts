// SPDX-License-Identifier: Apache-2.0
//
// What a run's own steps show it did, for a receipt whose outcome reports no
// changed files or checks. It reads the step groups the receipt already shows.

import type { ActivityStatus, NotRun } from "../../protocol/types";
import { stepSummary, type Step } from "../talk/timeline";
import { toolInput, toolKind } from "../talk/toolInput";

export interface ObservedWork {
  /** `files`: paths the edit steps name; `unnamed`: the lines of edit steps
   *  that name none. A failed edit changed nothing and is left out. */
  edits: { count: number; files: string[]; unnamed: string[] };
  /** `notRun`: why a failed command never ran. */
  commands: { key: number; text: string; status: ActivityStatus; notRun?: NotRun }[];
}

export function observedWork(groups: { steps: Step[]; ended: boolean }[]): ObservedWork {
  const edits: ObservedWork["edits"] = { count: 0, files: [], unnamed: [] };
  const commands: ObservedWork["commands"] = [];
  for (const { steps, ended } of groups) {
    for (const step of steps) {
      const { tool, detail, status } = step.item;
      const kind = toolKind(tool);
      const input = detail ? toolInput(tool, detail) : null;
      if (kind === "edit" && status !== "failed") {
        const paths = input?.text.split("\n").filter(Boolean) ?? [];
        edits.count++;
        if (paths.length) edits.files.push(...paths.filter(path => !edits.files.includes(path)));
        else edits.unnamed.push(stepSummary(step, ended));
      } else if (kind === "command") {
        // A run that ended with the command still running never reported its result.
        commands.push({ key: step.seq, text: input?.text ?? stepSummary(step, ended), status: status === "running" && ended ? "unknown" : status, notRun: step.item.not_run });
      }
    }
  }
  return { edits, commands };
}
