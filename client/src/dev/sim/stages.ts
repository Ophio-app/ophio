// SPDX-License-Identifier: Apache-2.0
import type { Command, CommandResult, ProjectStage, ProjectStageHistory, ProjectStageTransition, Query } from "../../protocol/types";
import type { SimHost } from "./host";
import { ids } from "./scenario";

const choices: ProjectStage[] = ["planning", "design", "prototype", "review", "implementation", "released", "paused"];
export class StageSim {
  readonly history = new Map<string, ProjectStageTransition[]>();
  failQuery = false;
  failChange = false;
  constructor(private readonly host: SimHost) {
    const project = host.projects[0];
    if (!project) return;
    const now = Date.now();
    const transitions: ProjectStageTransition[] = [
      { revision: 1, from: null, to: "planning", note: "Agree on the first workflow and its boundaries.", at: now - 4 * 86400_000, principal: { kind: "local_owner" }, actor_name: "Local owner" },
      { revision: 2, from: "planning", to: "design", note: "The project brief is ready for layout work.", at: now - 2 * 86400_000, principal: { kind: "device", device_id: ids.device }, actor_name: "Pixel 9" },
      { revision: 3, from: "design", to: "prototype", note: "Try the navigation with a small working example.", at: now - 3600_000, principal: { kind: "device", device_id: ids.device }, actor_name: "Pixel 9" },
    ];
    this.history.set(project.project_id, transitions);
    project.stage = transitions.at(-1);
  }
  query(q: Extract<Query, { kind: "project_stages" }>): { kind: "project_stages"; history: ProjectStageHistory } {
    if (this.failQuery) throw { code: "temporarily_unavailable", message: "Stage history is unavailable. Try again." };
    const project = this.host.projects.find(p => p.project_id === q.project_id);
    if (!project) throw { code: "not_found", message: "The project is no longer registered." };
    const entries = [...(this.history.get(q.project_id) ?? [])].reverse().filter(t => q.before_revision === null || t.revision < q.before_revision);
    const transitions = entries.slice(0, Math.max(1, Math.min(q.limit, 100)));
    return { kind: "project_stages", history: { project_id: q.project_id, current: project.stage ?? null, transitions, has_more: entries.length > transitions.length, stages: choices, note_limit: 500 } };
  }
  change(c: Extract<Command, { command: "set_project_stage" }>, expected: number | null, other = false): CommandResult {
    if (this.failChange) throw { code: "permission_denied", message: "This device no longer has permission to change project settings." };
    const project = this.host.projects.find(p => p.project_id === c.project_id);
    if (!project) throw { code: "not_found", message: "The project is no longer registered." };
    const revision = project.stage?.revision ?? 0;
    if (expected !== revision) throw { code: "stale_revision", message: "The project stage changed. Review it again.", detail: { current_revision: revision, current_stage: project.stage?.to ?? null, current: project.stage ?? null } };
    if (c.stage === project.stage?.to) throw { code: "conflict", message: "The project is already in this stage." };
    if (c.note && ([...c.note].length > 500 || /[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/u.test(c.note))) throw { code: "invalid_request", message: "The stage note is too long or contains control characters." };
    const transition: ProjectStageTransition = { revision: revision + 1, from: project.stage?.to ?? null, to: c.stage, note: c.note?.trim() || null, at: Date.now(), principal: other ? { kind: "device", device_id: "0192f7a1-0000-7000-8000-00000000d002" } : { kind: "device", device_id: ids.device }, actor_name: other ? "Another device" : "Pixel 9" };
    this.history.set(c.project_id, [...(this.history.get(c.project_id) ?? []), transition]);
    const updated = { ...project, stage: transition };
    this.host.projects = this.host.projects.map(p => p.project_id === c.project_id ? updated : p);
    this.host.emit({ type: "project_upserted", project: updated }, { kind: "companion" }, "observed");
    return { kind: "project_stage_set", project_id: c.project_id, transition };
  }
  anotherDevice(stage: ProjectStage = "review", projectId = this.host.projects[0]?.project_id): void {
    const project = this.host.projects.find(p => p.project_id === projectId);
    if (project) this.change({ command: "set_project_stage", project_id: project.project_id, stage, note: "Reviewed on another device." }, project.stage?.revision ?? 0, true);
  }
  unset(projectId = this.host.projects[0]?.project_id): void {
    const project = this.host.projects.find(p => p.project_id === projectId);
    if (!project) return;
    this.history.delete(project.project_id);
    const updated = { ...project, stage: undefined };
    this.host.projects = this.host.projects.map(p => p.project_id === project.project_id ? updated : p);
    this.host.emit({ type: "project_upserted", project: updated });
  }
}
