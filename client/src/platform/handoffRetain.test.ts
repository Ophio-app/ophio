// SPDX-License-Identifier: Apache-2.0
import { waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";
import type { Snapshot } from "../protocol/types";
import { profile, FakeLink, platform } from "./lifecycle.fixtures";
import * as fx from "../dev/sim/scenario";
import { Handoffs, switchRisk } from "./switching";

beforeEach(() => localStorage.clear());
afterEach(() => localStorage.clear());

function humanControl(): Snapshot["control"][number] {
  return { target_id: fx.ids.tgDp1, input_state: "human", lease: { lease_id: fx.ids.lease1, target_id: fx.ids.tgDp1, owner: { kind: "human", device_id: fx.ids.device }, generation: 1, boot_epoch: fx.ids.boot, state: "active", granted_at: 1, renewal_window_ms: 15000 }, model_observation: null, private_step: null };
}

it("leaving a computer with control again keeps its earlier unconfirmed Stop", async () => {
  // Before the app restarted, Stop on this task was sent and never confirmed.
  // Only the kept handoff remembers it: commands live in memory.
  localStorage.setItem("unresolved-computers.v1", JSON.stringify([{ profileId: profile.profileId, since: 1, busy: false, detail: "",
    risk: { leases: [], privateTargets: [], stoppingTasks: [fx.ids.tFlaky], pendingInput: [] } }]));
  const link = new FakeLink();
  // The person has since returned to this computer and taken control of its desktop.
  link.snap.control = [humanControl()];
  const p = platform();
  p.value.control = () => link;
  const handoffs = new Handoffs(p.value, [profile]);
  handoffs.restore();
  handoffs.activeProfileId = profile.profileId;
  const session = handoffs.sessions.get(profile.profileId)!;
  await waitFor(() => expect(session.state.get().caughtUp).toBe(true));
  expect(session.state.get().tasks[fx.ids.tFlaky]?.state).toBe("awaiting_decision");
  await waitFor(() => expect(handoffs.get()[0]?.risk.stoppingTasks).toEqual([fx.ids.tFlaky]));

  // Leaving again with control: the app retains what this session sees, as
  // App's "Leave control and switch" does, and releases control.
  const risk = switchRisk(session.state.get(), session.commands.get(), profile.deviceId);
  expect(risk.leases).toHaveLength(1);
  handoffs.retain(profile, session, risk);
  await handoffs.release(profile.profileId);
  expect(link.sent).toContainEqual({ command: "release_lease", lease_id: fx.ids.lease1 });

  // The control is released, but nothing confirmed the Stop and the task is
  // still running: the warning and its Stop button must stay.
  await waitFor(() => expect(handoffs.get()[0]?.risk.leases).toEqual([]));
  expect(handoffs.get()[0]?.risk.stoppingTasks).toEqual([fx.ids.tFlaky]);
  handoffs.dispose();
});
