// SPDX-License-Identifier: Apache-2.0
import { useCallback, useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { CHECKS_FEATURE } from "../../app/checksFeature";
import type { Task } from "../../protocol/types";
import type { CheckJob } from "../../protocol/generated/CheckJob";
import type { ChecksDraftView } from "../../protocol/generated/ChecksDraftView";

type TaskChecks = { jobs: CheckJob[]; drafts: ChecksDraftView[] };

/** Read computer-owned check evidence separately from the agent's report.
 * Refresh only while this receipt is visible, and poll unfinished proof jobs. */
export function useTaskChecks(task: Task, active: boolean) {
  const { session, profile } = useApp();
  const online = useLink().state === "online";
  const supported = useHost(s => s.capabilities?.features.includes(CHECKS_FEATURE) === true);
  const permitted = useHost(s => s.grants?.tasks === true);
  const [held, setHeld] = useState<{ taskId: string; data: TaskChecks } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reading, setReading] = useState(false);
  const [revision, setRevision] = useState(0);
  const refresh = useCallback(() => setRevision(n => n + 1), []);
  useEffect(() => {
    if (!active || !online || !supported || !permitted) {
      setReading(false);
      setError(null);
      return;
    }
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const read = async () => {
      setReading(true);
      try {
        const result = await session.query({ kind: "task_checks", task_id: task.task_id });
        if (!alive) return;
        setHeld({ taskId: task.task_id, data: result });
        setError(null);
        if (task.checking || result.jobs.some(j => ["pending", "running", "cancelling"].includes(j.state)) || result.drafts.some(d => d.draft.state === "proving")) {
          timer = setTimeout(() => void read(), 2000);
        }
      } catch (e) {
        if (alive) setError(describeError(e, profile.name));
      } finally {
        if (alive) setReading(false);
      }
    };
    void read();
    return () => { alive = false; clearTimeout(timer); };
  }, [active, online, supported, permitted, task.task_id, task.updated_at, task.checking?.job_id, task.checking?.done, task.checking?.total, session, profile.name, revision]);
  const data = supported && permitted && held?.taskId === task.task_id ? held.data : null;
  return { data, error, reading, supported, refresh };
}
