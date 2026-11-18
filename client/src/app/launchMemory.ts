// SPDX-License-Identifier: Apache-2.0
//
// The agent, model and effort the person usually uses for each kind of work,
// remembered on this phone, so a new task asks for the message and nothing
// else. The kinds are kept apart: a quick, cheap choice for finding something
// out never becomes the choice for a change.
//
// Only the person's own picks are remembered: on a suggestion's card, in the
// message box and on Review. Nothing an agent writes reaches here; a
// suggestion supplies its kind, never its agent.
//
// Look-up: what the person last chose for that kind in this project, then
// for that kind in any project, then the project's own default agent and the
// app's default agent (or, for a question or a plan, the first agent that
// can hold it read-only), at that model's highest effort. A remembered model or
// effort this computer no longer offers falls back to the agent's default for
// this task only; what is remembered stays, for when it is offered again.
//
// This key is new; nothing an earlier release reads is changed.

import type { AgentChoice } from "./ui";
import type { HarnessInfo, Project, TaskMode } from "../protocol/types";
import { defaultLaunchPolicy, launchChoices } from "./launchPolicy";
import { canStart, modeSupport, resolveAgent } from "../screens/talk/intent";
import { effortForModel, modelEfforts } from "../screens/talk/catalog";

/** What a piece of work is, from the work itself. */
export type WorkKind = "find" | "plan" | "change" | "fix" | "desktop";
export const WORK_KINDS: readonly WorkKind[] = ["find", "plan", "change", "fix", "desktop"];

/** The kind a task made by hand is: its mode, or the issue it fixes, or the desktop. */
export function kindOf(mode: TaskMode | null | undefined, opts: { issue?: boolean; desktop?: boolean } = {}): WorkKind {
  if (opts.desktop) return "desktop";
  if (opts.issue) return "fix";
  return mode === "ask" ? "find" : mode === "plan" ? "plan" : "change";
}

interface Stored {
  /** Per project, per kind. */
  projects?: Record<string, { kinds?: Partial<Record<WorkKind, AgentChoice>>; pr?: boolean }>;
  /** The last choice for each kind in any project. */
  any?: Partial<Record<WorkKind, AgentChoice>>;
}

const key = (profile: string) => `launch.usual.${profile}`;

function read(profile: string): Stored {
  try {
    const v = JSON.parse(localStorage.getItem(key(profile)) ?? "{}") as unknown;
    return v && typeof v === "object" && !Array.isArray(v) ? (v as Stored) : {};
  } catch {
    return {};
  }
}

function write(profile: string, s: Stored): void {
  try { localStorage.setItem(key(profile), JSON.stringify(s)); } catch { /* The choice still goes with this task. */ }
}

/** A stored choice, or nothing when it can't be read as one. */
function choice(v: unknown): AgentChoice | null {
  if (!v || typeof v !== "object") return null;
  const a = v as Record<string, unknown>;
  if (typeof a.harness !== "string" || !a.harness) return null;
  return {
    harness: a.harness as AgentChoice["harness"],
    model: typeof a.model === "string" ? a.model : null,
    effort: typeof a.effort === "string" ? a.effort : null,
    ...(a.launchPolicy === null || typeof a.launchPolicy === "string" ? { launchPolicy: a.launchPolicy as AgentChoice["launchPolicy"] } : {}),
  };
}

const plain = (a: AgentChoice): AgentChoice => ({
  harness: a.harness, model: a.model, effort: a.effort, ...(a.launchPolicy !== undefined ? { launchPolicy: a.launchPolicy } : {}),
});

/** Where a remembered choice came from, for tests and the picker. */
export type UsualFrom = "project" | "any" | "default";

/** What is remembered for this kind, before checking this computer offers it. */
export function remembered(profile: string, projectId: string, kind: WorkKind): { agent: AgentChoice; from: "project" | "any" } | null {
  const s = read(profile);
  const mine = choice(s.projects?.[projectId]?.kinds?.[kind]);
  if (mine) return { agent: mine, from: "project" };
  const any = choice(s.any?.[kind]);
  return any ? { agent: any, from: "any" } : null;
}

/** Remember the person's own pick for this kind, here and as the kind's last choice anywhere. */
export function rememberUsual(profile: string, projectId: string, kind: WorkKind, agent: AgentChoice): void {
  const s = read(profile);
  const projects = { ...(s.projects ?? {}) };
  const here = { ...(projects[projectId] ?? {}) };
  here.kinds = { ...(here.kinds ?? {}), [kind]: plain(agent) };
  projects[projectId] = here;
  write(profile, { ...s, projects, any: { ...(s.any ?? {}), [kind]: plain(agent) } });
}

/** Whether Work in this project gets its own branch and pull request where GitHub is on. */
export function rememberedPr(profile: string, projectId: string): boolean {
  const pr = read(profile).projects?.[projectId]?.pr;
  return typeof pr === "boolean" ? pr : true;
}

export function rememberPr(profile: string, projectId: string, pr: boolean): void {
  const s = read(profile);
  const projects = { ...(s.projects ?? {}) };
  projects[projectId] = { ...(projects[projectId] ?? {}), pr };
  write(profile, { ...s, projects });
}

const highest = (h: HarnessInfo | undefined, model: string | null): string | null => {
  if (h?.capabilities.effort_selection.state !== "available") return null;
  const efforts = modelEfforts(h, model);
  return efforts.length ? efforts[efforts.length - 1]!.id : null;
};

/**
 * A remembered choice as it will really run on this computer: a model it no
 * longer offers becomes the agent's default, an effort it doesn't offer for
 * that model becomes the agent's default, and a permission setting the agent
 * doesn't have becomes the person's default for it. Null when the agent
 * itself can't start here.
 */
export function asOffered(a: AgentChoice, harnesses: HarnessInfo[]): AgentChoice | null {
  const h = harnesses.find((x) => x.kind === a.harness);
  if (!h || !canStart(h)) return null;
  const model = a.model && h.capabilities.model_selection.state === "available" && h.models.some((m) => m.id === a.model) ? a.model : null;
  const effort = a.model && !model ? null : effortForModel(h, model, a.effort);
  const policy = a.launchPolicy === undefined || (a.launchPolicy !== null && !launchChoices(a.harness).some((c) => c.value === a.launchPolicy))
    ? defaultLaunchPolicy(a.harness) : a.launchPolicy;
  return { harness: a.harness, model, effort, launchPolicy: policy };
}

/** The agent, model and effort this kind of task starts with here, and where that came from. */
export function usualAgent(profile: string, project: Project | undefined, kind: WorkKind, harnesses: HarnessInfo[]): { agent: AgentChoice; from: UsualFrom } | null {
  const r = project ? remembered(profile, project.project_id, kind) : null;
  const offered = r ? asOffered(r.agent, harnesses) : null;
  if (r && offered) return { agent: offered, from: r.from };
  let fallback = resolveAgent(project, harnesses, null);
  if (!fallback) return null;
  // A question or a plan needs an agent that holds it read-only: the first
  // ready one that does, when the default can't.
  const mode: TaskMode = kind === "find" ? "ask" : kind === "plan" ? "plan" : "work";
  const able = (x: HarnessInfo | undefined) => !!x && canStart(x) && modeSupport(x, mode).availability.state === "available";
  if (!able(harnesses.find((x) => x.kind === fallback!.harness))) {
    const other = harnesses.find(able);
    if (other) fallback = { harness: other.kind, model: null, effort: null, launchPolicy: defaultLaunchPolicy(other.kind) };
  }
  const h = harnesses.find((x) => x.kind === fallback!.harness);
  // The project's own default keeps its effort; otherwise the highest the model offers.
  const effort = fallback.effort ?? (kind === "desktop" ? null : highest(h, fallback.model));
  return { agent: { ...fallback, effort }, from: "default" };
}
