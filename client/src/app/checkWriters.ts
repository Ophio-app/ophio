// SPDX-License-Identifier: Apache-2.0
import { useEffect } from "react";
import type { HostSession } from "../host/session";
import { useStore } from "../state/store";
import { CHECKS_FEATURE } from "./checksFeature";

/** A project can be held by a check job without any task or agent run. Read
 * that job before naming the holder; a missing run alone is not proof. */
export function useCheckWriters(session: HostSession) {
  const enabled = useStore(session.state, s => !!s.capabilities?.features.includes(CHECKS_FEATURE) && !!s.grants?.tasks);
  const online = useStore(session.link, s => s.state === "online");
  const revision = useStore(session.state, s => s.journalHead);
  const writers = useStore(session.state, s => [...new Set([
    ...Object.values(s.projects).map(p => p.active_writer),
    ...Object.values(s.queues).map(q => q.writer),
  ].filter((id): id is string => !!id && !s.runs[id] && !Object.values(s.tasks).some(t => t.checking?.job_id === id)))].sort().join(","));
  useEffect(() => {
    if (!enabled || !online || !writers) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const read = async () => {
      let active = false;
      await Promise.all(writers.split(",").map(async id => {
        try {
          const { job } = await session.query({ kind: "check_job", job_id: id });
          if (cancelled || job.id !== id) return;
          active ||= ["pending", "running", "cancelling"].includes(job.state);
          session.state.set(s => ({ ...s, checkJobs: { ...s.checkJobs, [job.id]: job } }));
        } catch {
          // It may be a run not received yet, or an unknown future holder.
          // Keep the honest unknown-holder text, without a speculative label.
        }
      }));
      if (!cancelled && active) timer = setTimeout(() => { void read(); }, 2000);
    };
    timer = setTimeout(() => { void read(); }, 100);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [session, enabled, online, writers, revision]);
}
