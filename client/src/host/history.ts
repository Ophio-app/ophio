// SPDX-License-Identifier: Apache-2.0
import type { Snapshot } from "../protocol/types";
import type { HostState } from "../state/projection";

/** Reused fixture/restored IDs with different immutable run admission facts
 * identify a replacement history. Never append it to a cached incarnation. */
export function reconcileCachedHistory(state: HostState, snapshot: Snapshot): HostState {
  const replaced = new Set(snapshot.runs.filter(run => {
    const cached = state.runs[run.run_id];
    return cached && (cached.admitted_at !== run.admitted_at || cached.task_id !== run.task_id);
  }).flatMap(run => [run.task_id, state.runs[run.run_id]!.task_id]));
  if (!replaced.size) return state;
  return { ...state, timelines: Object.fromEntries(Object.entries(state.timelines).map(([id, timeline]) =>
    [id, replaced.has(id) ? { items: [], loaded: false, hasMore: true } : timeline])) };
}
