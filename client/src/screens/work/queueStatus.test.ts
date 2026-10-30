// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { buildBrief, digestSentences } from "./brief";
import { queue, runs, tasks } from "../../dev/sim/scenario";

it("counts waiting tasks separately from runs in progress", () => {
  const queued = { ...tasks(Date.now())[0]!, state: "accepted" as const, current_run: null };
  const waiting = { ...queue(Date.now()), items: [{ ...queue(Date.now()).items[0]!, task_id: queued.task_id, state: { state: "waiting" as const } }] };
  const result = buildBrief(queued.project_id, 0, [queued], [], [], Infinity, waiting);
  expect(result).toMatchObject({ remaining: 0, queued: 1 });
  expect(digestSentences(result).lead).toBe("1 task is waiting in Next up.");
  const running = { ...queued, task_id: "running", state: "working" as const, current_run: "running-run" };
  const run = { ...runs(Date.now())[0]!, run_id: "running-run", task_id: "running", state: "working" as const, ended_at: null };
  const mixed = buildBrief(queued.project_id, 0, [queued, running], [run], [], Infinity, waiting);
  expect(mixed).toMatchObject({ remaining: 1, queued: 1 });
  expect(digestSentences(mixed)).toMatchObject({ lead: "1 task is still in progress.", details: ["1 task is waiting in Next up."] });
});

it("does not count an accepted task without queue evidence as in progress or queued", () => {
  const task = { ...tasks(Date.now())[0]!, state: "accepted" as const, current_run: null };
  expect(buildBrief(task.project_id, 0, [task], [], [])).toMatchObject({ remaining: 0, queued: 0 });
});
