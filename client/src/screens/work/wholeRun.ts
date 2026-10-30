// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { useApp, useLink } from "../../app/context";
import type { ChangedFile, Run } from "../../protocol/types";

/** The run read whole while a receipt shows it: the computer's lists leave
 * out the diffs agents report. Read again when the run changes state. */
export function useWholeRun(run: Run | undefined, active: boolean): Run | null {
  const { session } = useApp();
  const link = useLink();
  const [whole, setWhole] = useState<Run | null>(null);
  const runId = run?.run_id;
  const state = run?.state;
  useEffect(() => {
    if (!runId || !active || link.state !== "online" || !session.readsHistory) return;
    let alive = true;
    // Without it the receipt still shows every changed file, without its diff.
    session.loadRun(runId).then((r) => { if (alive) setWhole(r); }, () => undefined);
    return () => { alive = false; };
  }, [runId, state, active, link.state, session]);
  return whole?.run_id === runId ? whole : null;
}

/** `files` with the diffs the whole run records for those that lack one. */
export function withReportedDiffs(files: ChangedFile[], whole: Run | null): ChangedFile[] {
  const reported = new Map((whole?.outcome?.changed_files ?? []).map((f) => [f.path, f.diff]));
  return files.map((f) => {
    const diff = reported.get(f.path);
    return f.diff || !diff ? f : { ...f, diff };
  });
}
