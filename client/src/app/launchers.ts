// SPDX-License-Identifier: Apache-2.0
//
// Agents, skills and the scratch pad a new task can start from. The catalog is
// read from the computer each time and never kept here: the phone sends back
// only an entry's source, id and digest, and the computer reads the file again
// before the task starts.

import { useCallback, useEffect, useState } from "react";
import { useApp } from "./context";
import { freshReview, type AgentChoice, type ReviewDetails, type UiStore } from "./ui";
import { launchChoices } from "./launchPolicy";
import { modelEfforts } from "../screens/talk/catalog";
import type { Catalog, CatalogEntry, CatalogSource, HarnessInfo, HarnessKind, LibraryRoot, Project, StartedFrom, Task, TaskStart } from "../protocol/types";

export { LAUNCHERS_FEATURE, LAUNCHERS_UPDATE } from "./launchersFeature";

/** Drafts made for a task on its own, when no project is chosen to fall back to. */
export const ONE_OFF_DRAFT = "one-off";

/** What Review shows for a task on its own before its folder exists. */
export const ONE_OFF_PROJECT: Project = {
  project_id: ONE_OFF_DRAFT,
  name: "On its own",
  host_path: "",
  default_harness: null,
  default_model: null,
  default_effort: null,
  active_writer: null,
  vcs: null,
  created_at: 0,
  one_off: true,
};

/** What a draft's new task starts from, with what Review shows of it. */
export interface LaunchStart {
  start: TaskStart;
  name: string;
  /** Where the entry was found; absent for the scratch pad. */
  source?: CatalogSource;
  hostPath?: string;
  /** For a start copied from another task, what that task started from. */
  from?: StartedFrom;
}

export const SCRATCH: LaunchStart = { start: { type: "scratch" }, name: "Scratch pad" };

export const SOURCE_LABEL: Record<CatalogSource, string> = {
  ophio: "Yours",
  claude_user: "Claude Code",
  claude_project: "This project · Claude Code",
  codex_user: "Codex",
  codex_project: "This project · Codex",
  ophio_setup: "Built into Øphio",
};

export function launchStart(entry: CatalogEntry): LaunchStart {
  return {
    start: { type: entry.kind, source: entry.source, id: entry.id, digest: entry.digest },
    name: entry.name,
    source: entry.source,
    hostPath: entry.host_path,
  };
}

/** Next up's start until another is chosen: whatever its source task
 * started from, which the computer copies as that task was given it. */
export function inheritedStart(task: Task): LaunchStart | null {
  if (!task.start) return null;
  return { start: { type: "same_as", task_id: task.task_id }, name: task.start.type === "scratch" ? "Scratch pad" : task.start.name, from: task.start };
}

/** Whether it starts from an agent, a skill or the scratch pad, copied from another task or not. */
export function startKind(s: LaunchStart): StartedFrom["type"] {
  return s.start.type === "same_as" ? s.from?.type ?? "scratch" : s.start.type;
}

/** "Agent: Video producer", as a task shows what it started from. */
export function startedLabel(s: StartedFrom | LaunchStart | null | undefined): string | null {
  if (!s) return null;
  const type = "start" in s ? startKind(s) : s.type;
  if (type === "scratch") return "Scratch pad";
  return `${type === "agent" ? "Agent" : "Skill"}: ${"name" in s ? s.name : ""}`;
}

/** Found in a project's own folders, so only a task in that project can start from it. */
export const projectSource = (source: CatalogSource | undefined): boolean => source === "claude_project" || source === "codex_project";

/** A one-off's folder name from what it's called, as the computer makes it
 * (`launchers::slug`): lower-case letters and digits joined by hyphens. */
export function slug(name: string): string {
  let out = "";
  for (const c of name.toLowerCase()) {
    if (/^[a-z0-9]$/.test(c)) out += c;
    else if (out && !out.endsWith("-")) out += "-";
    if (out.length >= 48) break;
  }
  out = out.replace(/^-+|-+$/g, "");
  return out || "task";
}

/** The harness that finds a skill by itself; another one is told to read its file. */
export function skillHarness(source: CatalogSource | undefined): HarnessKind | null {
  return source === "claude_user" || source === "claude_project" ? "claude_code"
    : source === "codex_user" || source === "codex_project" ? "codex" : null;
}

/** Set a new-task draft to start from `start` (or from nothing), with the agent it asks for. */
export function setDraftStart(ui: UiStore, projectId: string, start: LaunchStart | null, agent: AgentChoice | null = null): void {
  const prev = ui.draft(projectId, null)?.review;
  const review: ReviewDetails = { ...(prev ?? freshReview()), start };
  ui.saveDraft(projectId, null, { review, ...(agent ? { agent } : {}) });
}

/**
 * The agent, model, effort and flags an entry asks for, as far as this
 * computer offers them. A model or effort it doesn't list is left to the
 * agent's default rather than sent and refused.
 */
export function entryAgent(entry: CatalogEntry, harnesses: HarnessInfo[]): AgentChoice | null {
  if (!entry.harness) return null;
  const h = harnesses.find((x) => x.kind === entry.harness);
  if (!h) return null;
  const wanted = entry.model?.toLowerCase() ?? null;
  // Claude's agent files name a family ("opus"); the first listed is its newest.
  const model = !wanted ? null
    : h.models.find((m) => m.id.toLowerCase() === wanted)?.id
      ?? h.models.find((m) => m.id.toLowerCase().includes(wanted) || m.label.toLowerCase().includes(wanted))?.id
      ?? null;
  const efforts = h.models.some((m) => m.details) ? modelEfforts(h, model).map((e) => e.id) : h.efforts;
  const effort = entry.effort && efforts.includes(entry.effort) ? entry.effort : null;
  const policy = entry.launch_policy && launchChoices(h.kind).some((c) => c.value === entry.launch_policy) ? entry.launch_policy : undefined;
  return { harness: h.kind, model, effort, ...(policy !== undefined ? { launchPolicy: policy } : {}) };
}

/** The phone's own date, so a folder made tonight isn't named tomorrow. */
export function localDate(now = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

/** Where a one-off's folder is in Library, when a root holds it. */
export function oneOffFolder(project: Project, roots: LibraryRoot[]): { rootId: string; path: string } | null {
  const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "");
  const at = norm(project.host_path);
  for (const root of roots) {
    // A root limited to agent files doesn't hold the one-off's folder.
    if (root.only) continue;
    const base = norm(root.host_path);
    if (at.startsWith(`${base}/`)) return { rootId: root.root_id, path: at.slice(base.length + 1) };
  }
  return null;
}

export interface CatalogRead {
  catalog: Catalog | null;
  loading: boolean;
  failed: string | null;
  refresh: () => void;
}

/** The catalog as the computer reads it now, with `projectId`'s own entries. */
export function useCatalog(projectId: string | null, enabled: boolean): CatalogRead {
  const { session } = useApp();
  const [state, setState] = useState<Omit<CatalogRead, "refresh">>({ catalog: null, loading: false, failed: null });
  const [asked, setAsked] = useState(0);
  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    setState((s) => ({ ...s, loading: true, failed: null }));
    session.query({ kind: "catalog", project_id: projectId }).then(
      (r) => { if (!cancelled) setState({ catalog: r.catalog, loading: false, failed: null }); },
      (e: Error) => { if (!cancelled) setState((s) => ({ ...s, loading: false, failed: e.message || "The computer didn’t answer." })); },
    );
    return () => { cancelled = true; };
  }, [session, projectId, enabled, asked]);
  const refresh = useCallback(() => setAsked((n) => n + 1), []);
  return { ...state, refresh };
}
