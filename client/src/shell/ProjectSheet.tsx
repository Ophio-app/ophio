// SPDX-License-Identifier: Apache-2.0
//
// Switch project: Home first, then the projects, which a search narrows by
// name or folder. A draft stays with the project and task it was typed for.

import { useEffect, useState } from "react";
import { Check, FolderGit2, FolderPlus, House, ListChecks, MonitorSmartphone, Trash2 } from "lucide-react";
import { useApp, useHost, useUi, useLink, shallowEqual } from "../app/context";
import { Empty, ListRow, Section } from "../design/ListRow";
import { IconButton } from "../design/Button";
import { Pill } from "../design/Chip";
import { TextField } from "../design/Field";
import { Sheet } from "../design/Sheet";
import { PRODUCT_NAME } from "../brand";
import { Age } from "../design/Age";
import { isTaskRunning, openAttention, reviewAttention, tasksMovedAt } from "../state/projection";
import type { HostState } from "../state/projection";
import { PathText } from "../design/PathText";
import type { Project } from "../protocol/types";
import { DeleteOneOff, useDeleteOneOff } from "../screens/work/DeleteOneOff";
import { useWarning } from "../app/warnings";
import ProjectSettingsSheet from "../screens/work/ProjectSheet";

/** Each project's tasks working and queued, what needs the person,
 * `unread`: results finished or failed that they haven't looked at yet,
 * and `moved`: when one of its tasks last moved it (see `tasksMovedAt`). */
export function projectCounts(s: HostState) {
  const out: Record<string, { active: number; queued: number; attention: number; unread: number; moved: number }> = {};
  for (const p of Object.values(s.projects)) out[p.project_id] = { active: 0, queued: 0, attention: 0, unread: 0, moved: p.created_at };
  const moved = tasksMovedAt(s);
  for (const t of Object.values(s.tasks)) {
    const o = out[t.project_id];
    if (!o) continue;
    const queued = t.state !== "archived" && (s.queues[t.project_id]?.items.some(i => i.task_id === t.task_id && i.state.state !== "started") || (t.state === "accepted" && !isTaskRunning(s, t)));
    if (queued) o.queued++;
    else if (isTaskRunning(s, t)) o.active++;
    o.moved = Math.max(o.moved, moved[t.task_id] ?? 0);
  }
  const projectOf = (a: { task_id: string | null }) => {
    const task = a.task_id ? s.tasks[a.task_id] : undefined;
    return task ? out[task.project_id] : undefined;
  };
  for (const a of openAttention(s)) {
    const project = projectOf(a);
    if (project) project.attention++;
  }
  for (const a of reviewAttention(s)) {
    const project = projectOf(a);
    if (project) project.unread++;
  }
  return out;
}

const ELSEWHERE_INPUTS = ["attention", "tasks", "approvals", "runs", "control", "targets", "projects"] as const;
let elsewhereMemo: { inputs: Pick<HostState, (typeof ELSEWHERE_INPUTS)[number]>; current: string | null; count: number } | undefined;

/** What waits in projects other than `current`: needs and unread results.
 * The header reads it on every update, words streaming included, so it is
 * counted again only when what it counts changes. */
export function elsewhere(s: HostState, current: string | null): number {
  const memo = elsewhereMemo;
  if (memo && memo.current === current && ELSEWHERE_INPUTS.every((k) => memo.inputs[k] === s[k])) return memo.count;
  const count = [...openAttention(s), ...reviewAttention(s)].filter((a) => {
    const task = a.task_id ? s.tasks[a.task_id] : undefined;
    return task !== undefined && task.project_id !== current && s.projects[task.project_id] !== undefined;
  }).length;
  const { attention, tasks, approvals, runs, control, targets, projects } = s;
  elsewhereMemo = { inputs: { attention, tasks, approvals, runs, control, targets, projects }, current, count };
  return count;
}

/** A count of unread results by a project's name. */
export function Unread({ count }: { count: number }) {
  if (count <= 0) return null;
  return (
    <span className="unread-count">
      {count}
      <span className="visually-hidden"> unread</span>
    </span>
  );
}

/** What `query` leaves of `projects`, by name or folder. */
export function matchingProjects(projects: Project[], query: string): Project[] {
  const q = query.trim().toLowerCase();
  return q ? projects.filter((p) => p.name.toLowerCase().includes(q) || p.host_path.toLowerCase().includes(q)) : projects;
}

/** Drops from the header as a search field over the list. The field isn't
 * focused on opening, so the keyboard comes up only when it's tapped. */
export function ProjectSheet({ open, onClose, allowAll = false }: { open: boolean; onClose: () => void; allowAll?: boolean }) {
  const { profile } = useApp();
  // Adding a project replaces the list with the add sheet, which closes both.
  const [adding, setAdding] = useState(false);
  const [query, setQuery] = useState("");
  useEffect(() => { if (!open) { setAdding(false); setQuery(""); } }, [open]);
  if (open && adding) return <ProjectSettingsSheet project={undefined} onClose={() => { setAdding(false); onClose(); }} />;
  // Counting every project's tasks on each store update is work only an open sheet shows.
  return (
    <Sheet open={open} onClose={onClose} title="Projects" subtitle={`On ${profile.name}`} size="drop" closeLabel="Close projects"
      head={<TextField label="Search projects" hideLabel placeholder="Search projects" type="search" value={query} onChange={setQuery} enterKeyHint="search" />}>
      <ProjectList onClose={onClose} onAdd={() => setAdding(true)} query={query} allowAll={allowAll} />
    </Sheet>
  );
}

function ProjectList({ onClose, onAdd, query, allowAll }: { onClose: () => void; onAdd: () => void; query: string; allowAll: boolean }) {
  const { profile } = useApp();
  const { ui } = useApp();
  const projects = useHost((s) => Object.values(s.projects), shallowEqual);
  const counts = useHost(projectCounts, (a, b) => JSON.stringify(a) === JSON.stringify(b));
  const online = useLink().state === "online";
  const heard = useHost(s => s.lastHeardAt);
  const current = useUi((s) => s.projectId);
  const all = useUi((s) => s.todoAll);
  const drafts = useUi((s) => s.drafts);
  const admin = useHost((s) => s.grants?.admin === true);
  const [deleting, setDeleting] = useState<Project | null>(null);
  const warn = useWarning("project.delete");
  const remove = useDeleteOneOff();
  const caughtUp = useHost((s) => s.caughtUp);
  const heardHost = useHost((s) => s.host !== null);
  const sorted = [...projects].sort((a, b) => (counts[b.project_id]?.moved ?? 0) - (counts[a.project_id]?.moved ?? 0));
  const home = sorted.find((p) => p.home);
  const shown = matchingProjects(sorted, query);
  const searching = !!query.trim();
  // Home leads; tasks started on their own are kept apart, so they don't crowd the projects.
  const places = shown.filter((p) => !p.one_off && !p.home);
  const oneOffs = shown.filter((p) => p.one_off);

  const deleted = (gone: Project) => {
    if (current === gone.project_id) {
      const next = home ?? sorted.find((p) => !p.one_off && p.project_id !== gone.project_id)
        ?? sorted.find((p) => p.project_id !== gone.project_id);
      if (next) ui.selectProject(next.project_id);
    }
    setDeleting(null);
  };

  if (deleting)
    return (
      <Section title="Delete one-off">
        <DeleteOneOff
          project={deleting}
          onCancel={() => setDeleting(null)}
          onDeleted={() => deleted(deleting)}
        />
      </Section>
    );

  const row = (p: Project) => {
    const c = counts[p.project_id];
    const hasDraft = Object.values(drafts).some((d) => d.projectId === p.project_id);
    return (
      <ListRow
        key={p.project_id}
        icon={p.home ? House : p.one_off ? FolderPlus : FolderGit2}
        title={<>{p.name}<Unread count={c?.unread ?? 0} /></>}
        subtitle={
          <>
            <PathText path={p.host_path} />
            {p.vcs?.branch ? ` · ${p.vcs.branch}` : ""}
            {!online && <><br />Last reported {heard === null ? "at an unknown time" : <Age at={heard} suffix=" ago" />}</>}
          </>
        }
        selected={p.project_id === current && (!allowAll || !all)}
        trailing={
          <span className="row__pills">
            {c && c.attention > 0 && <Pill tone="warn">{c.attention} {c.attention === 1 ? "needs" : "need"} you</Pill>}
            {c && c.active > 0 && <Pill tone={online ? "accent" : "neutral"}>{c.active} working</Pill>}
            {c && c.queued > 0 && <Pill>{c.queued} queued</Pill>}
            {hasDraft && <Pill>Draft</Pill>}
            {p.project_id === current && (!allowAll || !all) && <Check className="row__check" aria-label="Current" />}
          </span>
        }
        onClick={() => {
          ui.selectProject(p.project_id);
          onClose();
        }}
      />
    );
  };

  const homeShown = home && shown.includes(home);
  return (
    <>
      {!projects.length && !searching && (
        <Empty icon={FolderGit2} title={caughtUp ? "No projects yet" : `Waiting for ${profile.name}`}>
          {caughtUp
            ? "A project is a folder on the computer that agents may work in. Add one below to get started."
            : "Its projects appear once it answers."}
        </Empty>
      )}
      {allowAll && !searching && <Section><div className="group"><ListRow icon={ListChecks} title="All projects"
        subtitle="To-dos from every project" selected={all} trailing={all && <Check className="row__check" aria-label="Current" />}
        onClick={() => { ui.setTodoAll(true); onClose(); }} /></div></Section>}
      {(homeShown || places.length > 0 || (!home && heardHost && !searching)) && (
        <Section>
          <div className="group">
            {home ? homeShown && row(home) : heardHost && !searching && (
              // A computer from before Home lists only its projects.
              <ListRow icon={House} title="Home" subtitle={`Update ${PRODUCT_NAME} on ${profile.name} to work in its home folder`} disabled />
            )}
            {places.map(row)}
          </div>
        </Section>
      )}
      {searching && shown.length === 0 && <p className="fine">No project matches “{query.trim()}”.</p>}
      {oneOffs.length > 0 && (
        <Section title="One-offs">
          <div className="group">
            {oneOffs.map((p) => (
              <div key={p.project_id} className="project-row">
                {row(p)}
                {admin && <IconButton icon={Trash2} label={`Delete ${p.name}`} disabled={remove.pending}
                  onClick={() => warn.ask(() => setDeleting(p), () => void remove.run(p, () => deleted(p)))} />}
              </div>
            ))}
          </div>
        </Section>
      )}
      {!searching && <Section>
        <div className="group">
          <ListRow
            icon={FolderPlus}
            title="Add project"
            subtitle="Choose a folder on the computer"
            chevron
            onClick={onAdd}
          />
          <ListRow
            icon={MonitorSmartphone}
            title="Connections and readiness"
            subtitle="Paired devices, chat links and what’s ready to use"
            chevron
            onClick={() => {
              onClose();
              ui.push({ screen: "connections" });
            }}
          />
        </div>
      </Section>}
    </>
  );
}
