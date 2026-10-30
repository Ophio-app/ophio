// SPDX-License-Identifier: Apache-2.0
import type {
  AnnotationBundle,
  ArtifactMeta,
  ChildAgent,
  HarnessInfo,
  LibraryRoot,
  Project,
  QueueItem,
  Run,
  Task,
  TaskMode,
  TimelineItem,
} from "../../protocol/types";
import { effortLabel } from "../talk/intent";
import { modelName } from "../talk/catalog";

export const taskWords: Record<Task["state"], string> = {
  accepted: "Waiting to start",
  dispatching: "Starting",
  working: "Working",
  awaiting_decision: "Needs you",
  interrupted: "Stopped",
  completed: "Finished",
  failed: "Failed",
  reconciliation_required: "Outcome unknown",
  archived: "Archived",
};
export function decisionLabel(run?: Run): string | null {
  return run?.human_resolution ? run.human_resolution.resolution === "mark_failed" ? "Marked failed" : "Marked completed" : null;
}
export function hasEndedTask(task: Task): boolean {
  return (
    task.state === "completed" ||
    task.state === "failed" ||
    task.state === "interrupted"
  );
}
/** Whether a task's title, request or status says what was searched for. */
export function taskMatches(task: Task, search: string): boolean {
  const query = search.trim().toLocaleLowerCase();
  return !query || [task.title, task.mission?.goal ?? "", task.status_line ?? ""].some((text) => text.toLocaleLowerCase().includes(query));
}
/** Mode a run was admitted in. A run recorded before the host kept it takes
 * the task's mode only while it is the task's current run: the task may have
 * changed mode since an earlier one, so that one's mode is unknown. */
export function runMode(task: Task, run?: Run): TaskMode | null {
  if (run?.mode) return run.mode;
  return !run || run.run_id === task.current_run ? task.mode ?? "work" : null;
}
/** A read-only turn asked of a task in another mode, such as Summarize. It
 * answers in the task's own session and leaves the task as it was. */
export function sideRun(task: Task, run?: Run | null): boolean {
  return run?.mode === "ask" && (task.mode ?? "work") !== "ask";
}
/** The run, or the task when the run is its current one, finished by proposing
 * a plan. A question asked of the plan since leaves it proposed. */
export function planProposed(task: Task, run?: Run): boolean {
  const current = !run || run.run_id === task.current_run;
  const state = current ? task.state : run.state;
  return state === "completed" && (current && sideRun(task, run) ? task.mode : runMode(task, run)) === "plan";
}
/** What the agent reported its finished turn came to, where it isn't simply
 * done: the word a task's badge and its receipt's headline use. Only while the
 * run is the task's latest word on it: a newer turn, or a person's decision,
 * says more. */
export function reportLabel(task: Task, run?: Run): "Needs you" | "Not finished" | null {
  if (run?.state !== "completed" || run.human_resolution) return null;
  if (run.run_id === task.current_run && task.state !== "completed") return null;
  const status = run.outcome?.report?.status;
  // Work it left for a wake it asked for is being watched, not left.
  if (status === "unfinished" && task.wake && run.run_id === task.current_run) return null;
  return status === "needs_you" ? "Needs you" : status === "unfinished" ? "Not finished" : null;
}
/** New run events can precede the task update; a missing explicit run is unknown, not an older result. */
export function latestRun(
  task: Task,
  runs: Record<string, Run>,
): Run | undefined {
  if (task.current_run && !runs[task.current_run]) return undefined;
  return latestRuns(runs).get(task.task_id);
}

/** Each task's most recently admitted run, found once per runs record:
 * task rows ask for theirs on every store update. */
const latestMemo = new WeakMap<Record<string, Run>, Map<string, Run>>();

function latestRuns(runs: Record<string, Run>): Map<string, Run> {
  let latest = latestMemo.get(runs);
  if (!latest) {
    latest = new Map();
    for (const r of Object.values(runs)) {
      const best = latest.get(r.task_id);
      if (!best || r.admitted_at > best.admitted_at || (r.admitted_at === best.admitted_at && r.run_id.localeCompare(best.run_id) > 0)) latest.set(r.task_id, r);
    }
    latestMemo.set(runs, latest);
  }
  return latest;
}
/** Pass the agent's name when it is shown beside the model. */
export function modelLabel(
  run: Run | undefined,
  harnesses: HarnessInfo[],
  task?: Task,
  agent?: string,
): string {
  if (!run?.actual_model && (task?.state === "accepted" || (run && ["admitted", "dispatching"].includes(run.state)))) {
    const model = run?.requested_model ?? task?.model;
    const effort = run?.requested_effort ?? task?.effort;
    return model ? `Requested ${displayModel(model, harnesses.find(h => h.kind === (run?.harness ?? task?.harness)), model, agent)}${effort ? ` · ${effortLabel(effort)}` : ""}` : "Model chosen at start";
  }
  if (!run?.actual_model) return "Model not reported";
  return displayModel(run.actual_model, harnesses.find((h) => h.kind === run.harness), "Model not reported", agent);
}
export function harnessLabel(kind: Task["harness"]): string {
  return { claude_code: "Claude Code", codex: "Codex", hermes: "Hermes", open_claw: "OpenClaw", pi: "Pi" }[kind];
}
export function childProvenance(child: ChildAgent, run: Run): string {
  return child.provenance === "native"
    ? `Reported by ${harnessLabel(run.harness)}`
    : child.provenance === "observed"
      ? "Observed on the computer"
      : "Inferred, not confirmed";
}
/** A run lists its running helpers and the ones that ended most recently. */
export function earlierHelpers(count: number): string {
  return `${count} earlier ${count === 1 ? "helper ended and isn’t" : "helpers ended and aren’t"} listed. The computer keeps the most recent.`;
}
/** What a run leaves in doubt, whatever the agent called its turn: checks
 * that failed, helpers or work still running, unknowns. The receipt's
 * headline, its outcome and the computer's notification all say these, so
 * nothing reads as done while one remains. */
export function runDoubts(run: Run): string[] {
  // Read for verification, a check counts by its latest run: a failure it
  // fixed is no doubt, and one never run again still is (as `doubts` on the
  // computer says).
  const failed = run.outcome?.verification?.failed ?? run.outcome?.checks.filter((c) => c.status === "failed").length ?? 0;
  const notRerun = run.outcome?.verification?.failed_not_rerun ?? 0;
  const active = run.children.filter((c) => c.state === "active").length;
  const unknown =
    run.children.some((c) => c.state === "unknown") ||
    !!run.outcome?.unknowns.length;
  const rest = run.outcome?.still_active.length ?? 0;
  return [
    failed ? `${failed} check${failed === 1 ? "" : "s"} failed` : "",
    notRerun ? `${notRerun} check${notRerun === 1 ? "" : "s"} failed and ${notRerun === 1 ? "wasn’t" : "weren’t"} run again` : "",
    active ? `${active} helper${active === 1 ? "" : "s"} still running` : "",
    rest ? "work still active" : "",
    unknown ? "unknowns remain" : "",
  ].filter(Boolean);
}
/** The final check evidence, separate from whether the agent stopped normally. */
export function verificationLine(run: Run): string | null {
  const verification = run.outcome?.verification;
  if (!verification || (!run.outcome?.changed_files.length && !run.outcome?.checks.length)) return null;
  switch (verification.status) {
    case "verified": return verification.passed === 1
      ? "Checked: the check after its last change passed."
      : `Checked: all ${verification.passed} checks after its last change passed.`;
    case "failed": return `${verification.failed} check${verification.failed === 1 ? "" : "s"} failed after its last change.`;
    case "unknown": return verification.unknown === 1
      ? "Not verified: 1 check after its last change has no result of its own."
      : verification.unknown > 1
        ? `Not verified: ${verification.unknown} checks after its last change have no results of their own.`
        : verification.failed_not_rerun
          ? "Not verified: a check that failed earlier wasn’t run again."
          : `Not verified. ${verification.incomplete ?? "Some of what it did wasn’t recorded."}`;
    case "not_checked": return "Not verified: no check ran after its last change.";
  }
}
export function followUpLine(run: Run): string | null {
  switch (run.follow_up?.reason) {
    case "changed_files": return "Needed a follow-up: a later run changed files after this one finished.";
    case "check_failed": return "Needed a follow-up: a check failed at the start of the next run.";
    default: return null;
  }
}
export function receiptHeadline(task: Task, run?: Run): string {
  const base = decisionLabel(run) ?? reportLabel(task, run) ?? (planProposed(task, run) ? "Plan proposed" : run && task.current_run !== run.run_id ? ({ admitted: "Waiting", dispatching: "Starting", working: "Working", awaiting_approval: "Needs you", awaiting_input: "Needs you", stopping: "Stopping", interrupted: "Stopped", completed: "Finished", failed: "Failed", reconciliation_required: "Outcome unknown" }[run.state]) : taskWords[task.state]);
  if (!run) return `${base} · Run details not reported`;
  const details = runDoubts(run);
  return details.length ? `${base} — ${details.join(" · ")}` : base;
}
export function latestBundle(items: TimelineItem[]): AnnotationBundle | null {
  for (const item of [...items].sort((a, b) => b.seq - a.seq))
    if (item.item.kind === "user_message" && item.item.bundle)
      return item.item.bundle;
  return null;
}
export interface MarkEvidence {
  mark: AnnotationBundle["marks"][number];
  comments: AnnotationBundle["comments"];
  before: AnnotationBundle["sources"][number] | undefined;
  after: ArtifactMeta | undefined;
  status: "Evidence found" | "Not verified" | "No after picture";
  reason: string;
}
/** A picture is associated only by the persisted task/run/bundle/mark reference. */
export function markEvidence(
  bundle: AnnotationBundle,
  run: Run,
  artifacts: ArtifactMeta[],
): MarkEvidence[] {
  return bundle.marks.map((mark) => {
    const before = bundle.sources.find((s) => s.key === mark.source_key);
    const after = artifacts.filter((a) =>
      a.media_type.startsWith("image/") && a.links?.some((link) =>
        link.task_id === run.task_id && link.run_id === run.run_id && link.role === "after" &&
        link.annotation?.bundle_id === bundle.bundle_id &&
        link.annotation.revision === bundle.revision && link.annotation.mark_ids.includes(mark.mark_id)),
    ).sort((a, b) => b.created_at - a.created_at || a.artifact_id.localeCompare(b.artifact_id))[0];
    return {
      mark, before, after,
      comments: bundle.comments.filter((c) => c.mark_ids.includes(mark.mark_id)),
      status: !after ? "No after picture" : mark.deleted || !before ? "Not verified" : "Evidence found",
      reason: !after ? "No after picture linked to this drawing"
        : mark.deleted ? "The original mark was removed; its comments are retained."
        : !before ? "The original picture is missing."
        : "Linked to this drawing. Inspect the picture to judge the change; it does not verify a check.",
    };
  });
}
export function displayModel(id: string | null, harness: HarnessInfo | undefined, fallback: string, agent?: string): string {
  return id ? modelName(id, harness, agent) : fallback;
}
export function recentAgentTasks(tasks: Task[], runs: Record<string, Run>, limit = 8): Task[] {
  return tasks.filter((t) => {
    const run = latestRun(t, runs);
    return hasEndedTask(t) && run?.ended_at != null && !run.outcome?.still_active.length &&
      !run.children.some((c) => c.state === "active" || c.state === "unknown");
  }).sort((a, b) => (latestRun(b, runs)?.ended_at ?? 0) - (latestRun(a, runs)?.ended_at ?? 0) ||
    a.task_id.localeCompare(b.task_id)).slice(0, limit);
}
export function dependencyError(
  items: QueueItem[],
  itemId: string,
  dependsOn: string[],
): string | null {
  if (dependsOn.includes(itemId)) return "An item cannot depend on itself.";
  if (new Set(dependsOn).size !== dependsOn.length)
    return "A dependency is listed twice.";
  const graph = new Map(
    items.map((i) => [
      i.item_id,
      i.item_id === itemId ? dependsOn : i.depends_on,
    ]),
  );
  if (dependsOn.some((id) => !graph.has(id)))
    return "A dependency has left Next up. Review the current list.";
  const active = new Set<string>();
  const seen = new Set<string>();
  const visit = (id: string): boolean => {
    if (active.has(id)) return true;
    if (seen.has(id)) return false;
    active.add(id);
    if ((graph.get(id) ?? []).some(visit)) return true;
    active.delete(id);
    seen.add(id);
    return false;
  };
  return [...graph.keys()].some(visit)
    ? "Those dependencies form a cycle. Each item must be able to finish before the next one."
    : null;
}
export function moveError(
  items: QueueItem[],
  itemId: string,
  position: number,
): string | null {
  const source = items.findIndex((i) => i.item_id === itemId);
  if (source < 0) return "That item has left Next up.";
  if (!Number.isInteger(position) || position < 0 || position >= items.length)
    return "Choose a position in this queue.";
  if (
    items[source]?.state.state === "started" ||
    items[position]?.state.state === "started"
  )
    return "A started item cannot be reordered.";
  return null;
}
export function hostPathError(path: string, os: string | null): string | null {
  if (!os)
    return "The computer hasn’t reported its operating system. Connect before adding a folder.";
  if (!path.trim()) return "Enter the folder’s absolute path on the computer.";
  if (/[\u0000-\u001f]/.test(path))
    return "The path cannot contain control characters.";
  if (/windows/i.test(os)) {
    if (!/^(?:[a-zA-Z]:[\\/]|\\\\[^\\/]+[\\/][^\\/]+)/.test(path))
      return "Use an absolute Windows path, such as C:\\Projects\\app or \\\\server\\share\\app.";
    if (
      /[<>"|?*]/.test(path) ||
      /(^|[\\/])\.\.?([\\/]|$)/.test(path) ||
      /:/.test(path.slice(2))
    )
      return "Use a Windows folder path without wildcards, device prefixes or relative segments.";
  } else if (!path.startsWith("/") || /(^|\/)\.\.?($|\/)/.test(path))
    return "Use an absolute path, such as /home/sam/Projects/app, without relative segments.";
  return null;
}
export function libraryLocation(
  path: string,
  project: Project | undefined,
  roots: LibraryRoot[],
): { rootId: string; path: string } | null {
  if (!project || !path || /(^|[\\/])\.\.($|[\\/])/.test(path)) return null;
  const normal = (s: string) => s.replace(/\\/g, "/").replace(/\/$/, "");
  const full = /^(?:\/|[A-Za-z]:[\\/])/.test(path)
    ? normal(path)
    : `${normal(project.host_path)}/${normal(path)}`;
  const windows = /^[A-Za-z]:/.test(full) || full.startsWith("//");
  const key = (s: string) => (windows ? s.toLowerCase() : s);
  const root = [...roots]
    .sort((a, b) => b.host_path.length - a.host_path.length)
    .find((r) => key(full).startsWith(`${key(normal(r.host_path))}/`));
  return root
    ? {
        rootId: root.root_id,
        path: full.slice(normal(root.host_path).length + 1),
      }
    : null;
}
