// SPDX-License-Identifier: Apache-2.0
import type { HostSession } from "../host/session";
import type { HostProfile } from "../host/link";
import { emptyHostState, type HostState } from "../state/projection";

type Cached = Pick<HostState, "host" | "hostId" | "projects" | "tasks" | "runs" | "timelines" | "libraryRoots" | "lastHeardAt">;
const key = (id: string) => `read-cache.v1.${id}`;
/** Cache contains observations only: never grants, leases, pending actions or approvals. */
export function cachedState(profile: HostProfile): HostState | null {
  try {
    const raw = localStorage.getItem(key(profile.profileId));
    if (!raw || raw.length > 2_000_000) return null;
    const value = JSON.parse(raw) as Partial<Cached>;
    if (value.hostId !== profile.hostId || value.host?.host_id !== profile.hostId || typeof value.lastHeardAt !== "number"
      || !value.projects || !value.tasks || !value.runs || !value.timelines || !Array.isArray(value.libraryRoots)) return null;
    return { ...emptyHostState, hostId: value.hostId, host: value.host, lastHeardAt: value.lastHeardAt,
      projects: value.projects, tasks: value.tasks, runs: value.runs, timelines: value.timelines, libraryRoots: value.libraryRoots };
  } catch { return null; }
}
export function cacheObservations(profile: HostProfile, session: HostSession): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  // Streamed text and pongs change only the time last heard, and a run's new
  // items only its conversation, so each part is serialized again only when
  // what it is made from changes. The text is what JSON.stringify of the
  // whole cache gives, field by field in the same order.
  const parts = new Map<keyof Cached, { from: unknown[]; json: string }>();
  const part = (name: keyof Cached, from: unknown[], value: () => unknown): string => {
    const last = parts.get(name);
    if (last && from.every((x, i) => x === last.from[i])) return last.json;
    const json = JSON.stringify(value());
    parts.set(name, { from, json });
    return json;
  };
  let recent: { from: HostState["tasks"]; tasks: HostState["tasks"] } | null = null;
  const write = () => {
    timer = undefined;
    // What the computer said, not what this device expects of it.
    const s = session.state.base();
    if (!s.caughtUp || s.hostId !== profile.hostId || !s.host) return;
    if (recent?.from !== s.tasks) recent = { from: s.tasks, tasks: Object.fromEntries(Object.entries(s.tasks).sort(([, a], [, b]) => b.updated_at - a.updated_at).slice(0, 200)) };
    const { tasks } = recent;
    const text = `{"host":${part("host", [s.host], () => s.host)},"hostId":${JSON.stringify(s.hostId)},"projects":${part("projects", [s.projects], () => s.projects)}`
      + `,"tasks":${part("tasks", [tasks], () => tasks)}`
      + `,"runs":${part("runs", [s.runs, tasks], () => Object.fromEntries(Object.entries(s.runs).filter(([, r]) => r.task_id in tasks)))}`
      + `,"timelines":${part("timelines", [s.timelines, tasks], () => Object.fromEntries(Object.entries(s.timelines).filter(([id]) => id in tasks).map(([id, t]) => [id, { ...t, items: t.items.slice(-80), hasMore: t.hasMore || t.items.length > 80 }])))}`
      + `,"libraryRoots":${part("libraryRoots", [s.libraryRoots], () => s.libraryRoots)},"lastHeardAt":${JSON.stringify(s.lastHeardAt)}}`;
    try { if (text.length <= 2_000_000) localStorage.setItem(key(profile.profileId), text); } catch { /* No cache is claimed on storage failure. */ }
  };
  const off = session.state.subscribe(() => { if (!timer) timer = setTimeout(write, 1000); });
  return () => { off(); clearTimeout(timer); write(); };
}
export function forgetCache(profileId: string): void { localStorage.removeItem(key(profileId)); localStorage.removeItem(`last-reached.${profileId}`); }
