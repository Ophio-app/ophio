// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { LeaseOwner, LeaseState, TargetControlView } from "../protocol/types";
import { ownership } from "./Ownership";

const paused = (owner: LeaseOwner, state: LeaseState = "revoked"): TargetControlView => ({
  target_id: "screen", input_state: "paused", model_observation: null, private_step: null,
  lease: { lease_id: "lease", target_id: "screen", owner, generation: 1, boot_epoch: "boot", state, granted_at: 0, renewal_window_ms: 3000 },
} as TargetControlView);

it("a person's control stopped at the computer says it ended, not that an agent paused", () => {
  const mine = ownership(paused({ kind: "human", device_id: "phone" }), "phone", () => "Agent");
  expect(mine.title).toBe("Your control ended");
  expect(mine.detail).toContain("stopped at the computer");
  expect(ownership(paused({ kind: "human", device_id: "tablet" }), "phone", () => "Agent").title).toBe("Control ended");
  expect(ownership(paused({ kind: "human", device_id: "phone" }, "expired"), "phone", () => "Agent").detail).toContain("timed out");
  expect(ownership(paused({ kind: "agent", run_id: "run" }), "phone", () => "Agent").title).toBe("Agent paused");
});
