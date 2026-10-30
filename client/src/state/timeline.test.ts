// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import type { EventEnvelope, TimelineItem } from "../protocol/types";
import { applyEvent, applyTimelinePage, emptyHostState } from "./projection";
import { rows } from "../screens/talk/timeline";

const message: TimelineItem = { seq: 10, at: 100, run_id: "run", item: { kind: "user_message", text: "Later", bundle: null, delivery: "queued_for_boundary" } };
const accepted: TimelineItem = { seq: 20, at: 200, run_id: "run", item: { kind: "delivery", message_seq: 10, state: "host_accepted" } };
const delivered: TimelineItem = { seq: 30, at: 300, run_id: "run", item: { kind: "delivery", message_seq: 10, state: "native_delivered" } };
const event: EventEnvelope = { seq: 30, at: 300, source: { kind: "companion" }, provenance: "native", event: { type: "timeline_appended", task_id: "task", item: delivered } };

it("applies live delivery to every projection consumer without mutating earlier state", () => {
  const initial = applyTimelinePage(emptyHostState, "task", [message], false);
  const next = applyEvent(initial, event, 300);
  expect(next.timelines.task!.items[0]!.item).toMatchObject({ delivery: "native_delivered" });
  expect(initial.timelines.task!.items[0]!.item).toMatchObject({ delivery: "queued_for_boundary" });
  expect(applyEvent(next, event, 400)).toBe(next);
});

it("retains an unloaded message's update and folds it when an older page arrives", () => {
  let state = applyTimelinePage(emptyHostState, "task", [delivered], true);
  expect(rows(state.timelines.task!.items)).toEqual([]);
  state = applyTimelinePage(state, "task", [message, accepted], false, true);
  expect(state.timelines.task!.items[0]!.item).toMatchObject({ delivery: "native_delivered" });
  state = applyTimelinePage(state, "task", [message, accepted], false, true);
  expect(rows(state.timelines.task!.items)).toMatchObject([{ kind: "user", key: "s10", delivery: "native_delivered" }]);
});
