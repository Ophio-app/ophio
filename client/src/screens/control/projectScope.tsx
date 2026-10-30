// SPDX-License-Identifier: Apache-2.0
//
// Control's Terminals and Previews belong to the project chosen in the
// header, as Work's tasks do (D-066); Screens are the whole computer's. What
// other projects hold is counted under the list and shown on request.

import type { ReactNode } from "react";
import { shallowEqual, useHost, useUi } from "../../app/context";

/** Whether something of `owner` belongs to the chosen project. With no project
 * chosen everything does, and Home also holds what belongs to none. */
export function inProject(owner: string | null, projectId: string | null, homeId: string | null): boolean {
  if (!projectId) return true;
  return owner === projectId || (owner === null && projectId === homeId);
}

/** The chosen project, and whether something of `owner` belongs to it. A
 * project the computer doesn't report, or not yet, scopes nothing. */
export function useProjectScope() {
  const chosen = useUi((s) => s.projectId);
  const projectId = useHost((s) => (chosen && s.projects[chosen] ? chosen : null));
  const homeId = useHost((s) => Object.values(s.projects).find((p) => p.home)?.project_id ?? null);
  const name = useHost((s) => (projectId ? s.projects[projectId]?.name : undefined));
  return { projectId, name, here: (owner: string | null) => inProject(owner, projectId, homeId) };
}

/** `items` by the project each belongs to, in name order, Home holding those
 * of none; then any whose project is gone, or of none on a computer without
 * Home. */
export function useByProject<T>(items: T[], owner: (item: T) => string | null): { id: string | null; name: string; items: T[] }[] {
  const names = useHost((s) => Object.fromEntries(Object.values(s.projects).map((p) => [p.project_id, p.name])), shallowEqual);
  const homeId = useHost((s) => Object.values(s.projects).find((p) => p.home)?.project_id ?? null);
  const groups = new Map<string | null, T[]>();
  for (const item of items) {
    const id = owner(item) ?? homeId;
    const key = id !== null && names[id] !== undefined ? id : null;
    groups.set(key, [...(groups.get(key) ?? []), item]);
  }
  return [...groups.entries()]
    .map(([id, items]) => ({ id, name: id === null ? "No project" : names[id]!, items }))
    .sort((a, b) => (a.id === null ? 1 : b.id === null ? -1 : a.name.localeCompare(b.name)));
}

/** "Show N in other projects", and once open, each project's own list under
 * its name. */
export function OtherProjects({ count, open, onToggle, children }: {
  count: number;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  if (count === 0) return null;
  return (
    <>
      <p className="fine control-elsewhere">
        <button type="button" className="linklike" aria-expanded={open} onClick={onToggle}>
          {open ? "Hide other projects" : `Show ${count} in other projects`}
        </button>
      </p>
      {open && children}
    </>
  );
}

/** One other project's list, under its name. */
export function OtherProject({ name, children }: { name: string; children: ReactNode }) {
  return (
    <section className="control-elsewhere__project" aria-label={name}>
      <h3 className="control-elsewhere__name">{name}</h3>
      {children}
    </section>
  );
}
