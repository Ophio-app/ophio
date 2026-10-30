// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import {
  applyEvent,
  applySnapshot,
  emptyHostState,
} from "../../state/projection";
import type {
  Event,
  EventEnvelope,
  TargetControlView,
  ControlTarget,
} from "../../protocol/types";
import { hostInfo } from "../../dev/sim/scenario";

const target: ControlTarget = {
  target_id: "current",
  kind: "output",
  label: "Display",
  native_ref: "output",
  geometry: {
    width: 1280,
    height: 720,
    scale: 1,
    rotation_degrees: 0,
    revision: 1,
  },
  capture: { state: "available" },
  input: { state: "available" },
};
const view: TargetControlView = {
  target_id: "current",
  input_state: "unknown",
  lease: null,
  model_observation: null,
  private_step: null,
  desktop_lock: "unknown",
};
const previous = {
  ...emptyHostState,
  targets: [target],
  control: { current: view, removed: { ...view, target_id: "removed" } },
};
const event = (value: Event, seq = 1): EventEnvelope => ({
  seq,
  at: 100,
  provenance: "observed",
  source: { kind: "companion" },
  event: value,
});

it("retires removed-target warnings on the authoritative inventory, retaining current unknowns", () => {
  const state = applyEvent(
    previous,
    event({ type: "targets_changed", targets: [target] }),
    100,
  );
  expect(state.control).toEqual({ current: view });
  expect(previous.control.removed).toBeDefined();
  expect(
    applyEvent(state, event({ type: "targets_changed", targets: [] }, 2), 101)
      .control,
  ).toEqual({});
});

it("a snapshot cannot retain orphan control state after a restart", () => {
  const state = applySnapshot(
    previous,
    {
      host: hostInfo(100),
      journal_head: 2,
      targets: [target],
      control: Object.values(previous.control),
      projects: [],
      tasks: [],
      runs: [],
      approvals: [],
      attention: [],
      library_roots: [],
    },
    100,
  );
  expect(state.control).toEqual({ current: view });
});

it("boot changes keep uncertainty until the computer reports which targets remain", () => {
  const state = applyEvent(
    previous,
    event({ type: "boot_epoch_changed", boot_epoch: "next" }),
    100,
  );
  expect(Object.keys(state.control)).toEqual(["current", "removed"]);
  expect(state.control.current?.input_state).toBe("unknown");
});

it("a later lock event cannot resurrect safety state for a removed target", () => {
  const state = applyEvent(
    previous,
    event({ type: "targets_changed", targets: [target] }),
    100,
  );
  const next = applyEvent(
    state,
    event(
      {
        type: "control_changed",
        view: { ...view, target_id: "removed", desktop_lock: "locked" },
      },
      2,
    ),
    101,
  );
  expect(next.control).toEqual({ current: view });
});
