// SPDX-License-Identifier: Apache-2.0
import { useCallback, useEffect, useRef, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { useRoute } from "../../shell/Screen";
import type { ProjectStageTransition } from "../../protocol/types";
import { writeLocal } from "./local";
import { readStageCache, stageRevision, type StageCache } from "./stages";

export function useStages(projectId: string) {
  const { session, profile } = useApp();
  const { active } = useRoute();
  const link = useLink();
  const project = useHost(s => s.projects[projectId]);
  const allowed = useHost(s => s.grants?.tasks === true || s.grants?.admin === true);
  const gaps = useHost(s => s.gaps.length);
  const key = `${profile.profileId}.stages.${projectId}.history`;
  const [cache, setCache] = useState<StageCache | null>(() => readStageCache(key));
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [retry, setRetry] = useState(0);
  const request = useRef(0);
  const revision = project ? stageRevision(project.stage) : null;
  const load = useCallback(async (before: number | null) => {
    const ticket = ++request.current;
    setLoading(true);
    setError(null);
    try {
      const r = await session.query({ kind: "project_stages", project_id: projectId, before_revision: before, limit: 40 });
      if (ticket !== request.current) return;
      const observedAt = Date.now();
      setCache(previous => {
        const entries = before === null ? r.history.transitions : [...(previous?.history.transitions ?? []), ...r.history.transitions];
        const unique = new Map<number, ProjectStageTransition>(entries.map(t => [t.revision, t]));
        const next: StageCache = { history: { ...r.history, transitions: [...unique.values()].sort((a, b) => b.revision - a.revision) }, name: project?.name ?? previous?.name ?? "Project", observedAt };
        writeLocal(key, next);
        return next;
      });
    } catch (e) {
      if (ticket === request.current) setError(describeError(e, profile.name));
    } finally {
      if (ticket === request.current) setLoading(false);
    }
  }, [session, projectId, project?.name, key, profile.name]);
  useEffect(() => {
    if (active && allowed && link.state === "online") void load(null);
    else setLoading(false);
    return () => { request.current++; };
  }, [active, allowed, link.state, revision, gaps, retry, load]);
  // The projection is authoritative after an event; a history query may still
  // be in flight. On a fresh offline launch only the cached observation exists.
  const observed = cache?.history.current;
  const current = observed && stageRevision(observed) > stageRevision(project?.stage)
    ? observed : project ? project.stage ?? null : observed ?? null;
  return { project, cache, current, loading, error,
    reload: () => setRetry(n => n + 1),
    older: () => { const before = cache?.history.transitions.at(-1)?.revision; if (before !== undefined) void load(before); },
  };
}
