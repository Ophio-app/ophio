// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { projects, runs, tasks } from "../../dev/sim/scenario";
import { portableBrief } from "./portable";

const at = 100_000_000;

it("a brief of a run a person marked completed says so, not that the task finished", () => {
  // The computer couldn't confirm the outcome; a person recorded Mark completed.
  const run = { ...runs(at)[1]!, state: "completed" as const, outcome: null, ended_at: at,
    human_resolution: { resolution: "mark_completed" as const, at, by: { kind: "device" as const, device_id: "phone" } } };
  const task = { ...tasks(at)[1]!, state: "completed" as const, current_run: run.run_id };
  const brief = portableBrief({ task, runs: [run], project: projects(at)[0], timeline: [], artifacts: [], historyComplete: true, evidenceComplete: true });
  expect(brief).not.toContain("Recorded state: Finished");
  expect(brief).toContain("Marked completed");
  expect(brief).toContain("not verified by the computer");
});
