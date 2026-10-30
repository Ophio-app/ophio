// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { runs, tasks } from "../../dev/sim/scenario";
import type { TimelineItem } from "../../protocol/types";
import { runDuration } from "./runDuration";
import { buildBrief, digestSentences } from "./brief";
it("a late decision cannot extend the agent’s duration", () => {
  const run = { ...runs(1_000_000)[0]!, admitted_at: 0, ended_at: 507_000 };
  const items: TimelineItem[] = [{ seq: 1, run_id: run.run_id, at: 507_000, item: { kind: "run_ended", run_id: run.run_id, state: "interrupted" } }];
  expect(runDuration(run, items)).toBe("8 min 27 s");
  expect(runDuration({ ...run, state: "completed", ended_at: 7_747_000 }, items)).toBe("8 min 27 s");
  items[0]!.item = { kind: "run_ended", run_id: run.run_id, state: "reconciliation_required" };
  expect(runDuration({ ...run, acknowledged_at: 1000, ended_at: 7_747_000 }, items)).toBe("At least 8 min 27 s");
});
it("identifies the task whose helper remains unconfirmed", () => {
  const task = { ...tasks(1_000_000)[0]!, title: "Draft a short status note", state: "completed" as const };
  const run = { ...runs(1_000_000)[0]!, task_id: task.task_id, state: "completed" as const, ended_at: 900_000, outcome: { summary: "Finished", changed_files: [], checks: [], still_active: [], unknowns: [], usage: null },
    children: [{ child_id: "helper", label: "A helper", native_ref: null, state: "unknown" as const, started_at: 1000, ended_at: null, provenance: "native" as const }] };
  const summary = digestSentences(buildBrief(task.project_id, 0, [task], [run], []));
  expect(summary.lead).toBe("A helper from “Draft a short status note” is unconfirmed.");
});

it("measures a working run to now without changing reconciliation evidence", () => {
  const run = { ...runs(100_000)[0]!, state: "working" as const, admitted_at: 0, acknowledged_at: 1000, ended_at: null };
  expect(runDuration(run, [], 60_000)).toBe("Running · 1 min so far");
  expect(runDuration(run, [], 125_000)).toBe("Running · 2 min so far");
  expect(runDuration({ ...run, state: "reconciliation_required" }, [], 125_000)).toBe("At least 1 s");
});
