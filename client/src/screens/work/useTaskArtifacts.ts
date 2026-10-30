// SPDX-License-Identifier: Apache-2.0
import type { TaskArtifactHistory } from "../../protocol/types";
import { useEffect, useState } from "react";
import { describeError, shallowEqual, useApp, useHost, useLink } from "../../app/context";

/** Rebuild the task's artifact index without relying on retained journal events. */
export function useTaskArtifacts(taskId: string, active: boolean, retry: number) {
  const { session, profile } = useApp();
  const link = useLink();
  const permitted = useHost((s) => s.grants === null ? null : s.grants.tasks && s.grants.transfers);
  const missingGrants = useHost((s) => (["tasks", "transfers"] as const).filter((grant) => s.grants?.[grant] === false), shallowEqual);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    if (!active || link.state !== "online" || permitted !== true) {
      setLoading(false);
      setError(null);
      return;
    }
    let alive = true;
    setLoading(true);
    setError(null);
    void (async () => {
      let before: string | null = null;
      const seen = new Set<string>();
      do {
        const { history }: { history: TaskArtifactHistory } = await session.query({ kind: "task_artifacts", task_id: taskId, before, limit: 100 });
        if (!alive) return;
        session.state.set((s) => ({ ...s, artifacts: {
          ...s.artifacts, ...Object.fromEntries(history.artifacts.map((a) => [a.artifact_id, a])),
        } }));
        before = history.next_before;
        if (before && seen.has(before)) throw new Error("The computer repeated an evidence page. Reload to try again.");
        if (before) seen.add(before);
      } while (before && alive);
    })().catch((e: unknown) => {
      if (alive) setError(describeError(e, profile.name));
    }).finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; };
  }, [active, permitted, link.state, session, profile.name, taskId, retry]);
  return { error: permitted === true ? error : null, loading, missingGrants };
}
