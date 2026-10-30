// SPDX-License-Identifier: Apache-2.0
//
// What agents showed in a project, for the capture inbox, however long ago
// their tasks ended: the pictures and videos, and the project's pages. The
// computer keeps each a week after the person last opened it; a page the
// person added stays until they remove it.

import type { ArtifactMeta, PreviewEndpoint, TaskArtifactHistory } from "../../protocol/types";
import { useEffect, useState } from "react";
import { describeError, shallowEqual, useApp, useHost, useLink } from "../../app/context";
import type { HostState } from "../../state/projection";

export const PROJECT_SHOWN_FEATURE = "project_shown_v1";

const NONE: ReadonlySet<string> = new Set();
const NO_PAGES: PreviewEndpoint[] = [];

/** The pictures and videos agents showed in the project's tasks, newest
 * first: those the computer listed, and those shown since. */
export function projectShown(s: HostState, projectId: string, listed: ReadonlySet<string>): ArtifactMeta[] {
  return Object.values(s.artifacts)
    .filter((a) => /^(image|video)\//.test(a.media_type) && (listed.has(a.artifact_id) ||
      !!a.links?.some((l) => l.role === "shown" && s.tasks[l.task_id]?.project_id === projectId)))
    .sort((a, b) => b.created_at - a.created_at);
}

/** The task an agent showed it in, for drawing or talking over it. */
export function shownIn(a: ArtifactMeta): string {
  return a.links?.find((l) => l.role === "shown")?.task_id ?? a.task_id ?? "";
}

export function useProjectShown(projectId: string | null, retry = 0) {
  const { session, profile } = useApp();
  const link = useLink();
  const supported = useHost((s) => !!s.capabilities?.features.includes(PROJECT_SHOWN_FEATURE));
  const permitted = useHost((s) => s.grants === null ? null : s.grants.tasks && s.grants.transfers);
  const [listed, setListed] = useState<{ projectId: string; ids: ReadonlySet<string> } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    if (!projectId || !supported || link.state !== "online" || permitted !== true) {
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
      const ids = new Set<string>();
      do {
        const { history }: { history: TaskArtifactHistory } = await session.query({ kind: "project_shown", project_id: projectId, before, limit: 100 });
        if (!alive) return;
        session.state.set((s) => ({ ...s, artifacts: {
          ...s.artifacts, ...Object.fromEntries(history.artifacts.map((a) => [a.artifact_id, a])),
        } }));
        for (const a of history.artifacts) ids.add(a.artifact_id);
        setListed({ projectId, ids: new Set(ids) });
        before = history.next_before;
        if (before && seen.has(before)) throw new Error("The computer repeated a page of pictures. Reload to try again.");
        if (before) seen.add(before);
      } while (before && alive);
    })().catch((e: unknown) => {
      if (alive) setError(describeError(e, profile.name));
    }).finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; };
  }, [projectId, supported, permitted, link.state, session, profile.name, retry]);
  const ids = listed?.projectId === projectId ? listed.ids : NONE;
  const media = useHost((s) => projectId ? projectShown(s, projectId, ids) : [], shallowEqual);
  return { media, error: permitted === true ? error : null, loading, supported, permitted };
}

/** The pages an agent or the person added to the project, read once if
 * nothing else has read them yet. A site found running by itself comes and
 * goes with its server, so it is Control's to list, not the inbox's. One an
 * agent showed stays a week after the person last opened it. */
export function useProjectPages(projectId: string | null) {
  const { session } = useApp();
  const link = useLink();
  const loaded = useHost((s) => !!projectId && projectId in s.previews);
  const pages = useHost((s) => (projectId && s.previews[projectId] || NO_PAGES)
    .filter((p) => !p.automatic), shallowEqual);
  useEffect(() => {
    if (!projectId || loaded || link.state !== "online") return;
    let alive = true;
    void session.query({ kind: "previews", project_id: projectId }).then((r) => {
      if (alive) session.state.set((s) => projectId in s.previews ? s : { ...s, previews: { ...s.previews, [projectId]: r.previews } });
    }, () => undefined);
    return () => { alive = false; };
  }, [projectId, loaded, link.state, session]);
  return pages;
}
