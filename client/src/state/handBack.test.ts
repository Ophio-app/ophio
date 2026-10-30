// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { control, runs, targets, tasks } from "../dev/sim/scenario";
import type { TargetControlView } from "../protocol/types";
import { handBackTarget } from "./handBack";
import { emptyHostState, type HostState } from "./projection";

function released(): HostState {
  const target = targets()[0]!;
  const task = { ...tasks(1000)[0]!, target_id: target.target_id, state: "awaiting_decision" as const, status_line: "You took over DP-1" };
  const run = { ...runs(1000)[0]!, task_id: task.task_id, run_id: task.current_run!, state: "working" as const, ended_at: null };
  const [view] = control(1000);
  return { ...emptyHostState, tasks: { [task.task_id]: task }, runs: { [run.run_id]: run }, targets: [target],
    control: { [target.target_id]: { ...view!, target_id: target.target_id, input_state: "paused", private_step: null,
      lease: { ...view!.lease!, state: "revoked", owner: { kind: "human", device_id: "phone" } },
      model_observation: { ...view!.model_observation!, run_id: run.run_id } } } };
}
const only = (s: HostState) => Object.values(s.tasks)[0]!;
const withView = (s: HostState, change: Partial<TargetControlView>): HostState => {
  const [id, view] = Object.entries(s.control)[0]!;
  return { ...s, control: { [id]: { ...view, ...change } } };
};

it("a message after control ended leaves the hand-back due", () => {
  const s = released();
  expect(handBackTarget(s, only(s))?.target_id).toBe(only(s).target_id);
  // A message moves the task on like any turn: working, with new status text.
  for (const status_line of ["Adjusting to your message", "Working"])
    expect(handBackTarget(s, { ...only(s), state: "working", status_line })?.label).toBe("DP-1");
});

it("Pause input on the agent's own lease waits for it the same way", () => {
  const s = released();
  const run = only(s).current_run!;
  const paused = withView(s, { lease: { ...Object.values(s.control)[0]!.lease!, owner: { kind: "agent", run_id: run } } });
  expect(handBackTarget(paused, { ...only(s), state: "working", status_line: "Working" })?.label).toBe("DP-1");
  const other = withView(s, { lease: { ...Object.values(s.control)[0]!.lease!, owner: { kind: "agent", run_id: "another-run" } } });
  expect(handBackTarget(other, only(s))).toBeUndefined();
});

it("giving input back, taking over again, the run ending or a private step clears it", () => {
  const s = released();
  const task = only(s);
  expect(handBackTarget(withView(s, { input_state: "agent" }), task)).toBeUndefined();
  expect(handBackTarget(withView(s, { input_state: "human" }), task)).toBeUndefined();
  expect(handBackTarget(withView(s, { input_state: "ready", lease: null }), task)).toBeUndefined();
  expect(handBackTarget(withView(s, { model_observation: null }), task)).toBeUndefined();
  const run = s.runs[task.current_run!]!;
  expect(handBackTarget({ ...s, runs: { [run.run_id]: { ...run, state: "completed", ended_at: 2000 } } }, task)).toBeUndefined();
  expect(handBackTarget({ ...s, runs: { [run.run_id]: { ...run, state: "stopping" } } }, task)).toBeUndefined();
  const step = { requested_at: 1500 } as NonNullable<TargetControlView["private_step"]>;
  expect(handBackTarget(withView(s, { private_step: step }), task)).toBeUndefined();
});
