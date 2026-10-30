// SPDX-License-Identifier: Apache-2.0
import { expect, it } from "vitest";
import { applyTimelinePage, emptyHostState } from "../state/projection";
import type { Snapshot, TimelineItem } from "../protocol/types";
import { runs } from "../dev/sim/scenario";
import { reconcileCachedHistory } from "./history";
const item = (seq: number, text: string): TimelineItem => ({ seq, at: seq * 100, run_id: "run", item: { kind: "note", text } });
it("merges interleaved cached and fresh items in journal order by identity", () => {
  const cached = applyTimelinePage(emptyHostState, "task", [item(7, "seven"), item(3, "old three")], false);
  const fresh = applyTimelinePage(cached, "task", [item(5, "five"), item(3, "fresh three"), item(2, "two")], false);
  expect(fresh.timelines.task?.items).toEqual([item(2, "two"), item(3, "fresh three"), item(5, "five"), item(7, "seven")]);
  expect(applyTimelinePage(fresh, "task", [item(3, "fresh three")], false).timelines.task?.items).toHaveLength(4);
});
it("a replacement run history cannot append to a reused cached identity", () => {
  const run = runs(100_000)[0]!;
  const cached = { ...applyTimelinePage(emptyHostState, run.task_id, [item(7, "old")], false), runs: { [run.run_id]: run } };
  const snapshot = { runs: [{ ...run, admitted_at: run.admitted_at + 1 }] } as Snapshot;
  expect(reconcileCachedHistory(cached, snapshot).timelines[run.task_id]).toEqual({ items: [], loaded: false, hasMore: true });
  expect(reconcileCachedHistory(cached, { runs: [run] } as Snapshot)).toBe(cached);
});
