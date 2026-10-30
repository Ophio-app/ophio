// SPDX-License-Identifier: Apache-2.0
import type { Approval, ArtifactMeta, HarnessInfo, Project, Run, Task, TimelineItem } from "../../protocol/types";
import { decisionLabel, planProposed, taskWords } from "./logic";
import { stopFactWord, stopProgress, stopReceipt, stopUnknowns } from "./stopProgress";

const quote = (value: string) => value.split("\n").map((line) => `> ${line}`).join("\n");
const line = (value: string) => value.replaceAll("\n", " ").replaceAll("\r", " ");
const date = (at: number) => new Date(at).toISOString();
export interface BriefInput {
  task: Task;
  project?: Project;
  runs: Run[];
  timeline: TimelineItem[];
  artifacts: ArtifactMeta[];
  approvals?: Approval[];
  evidenceComplete?: boolean;
  historyComplete: boolean;
}
/** Same recorded input produces the same Markdown, without a clock or inference call. */
export function portableBrief({ task, project, runs, timeline, artifacts, approvals = [], historyComplete, evidenceComplete = false }: BriefInput): string {
  const history = [...timeline].sort((a, b) => a.seq - b.seq);
  const messages = history.filter((i) => i.item.kind === "user_message");
  const initial = messages[0]?.item;
  const goal = task.mission?.goal ?? (initial?.kind === "user_message" ? initial.text : task.title);
  const out = [
    `# Task brief: ${line(task.title)}`, "",
    "This is context for a new task. Review the goal and boundaries before starting it.", "",
    "## Goal", "", quote(goal), "", "## Decisions and boundaries", "",
    ...(task.mission?.bounds.map((b) => `- ${line(b.text)} (${b.enforcement === "enforced" ? "enforced by the agent’s tools" : b.enforcement === "requested_only" ? "requested, not enforced" : "not supported"})`) ?? []),
    task.mission?.stop_point ? `Stop when requested: ${line(task.mission.stop_point)}` : "No stopping point was recorded.",
  ];
  for (const message of messages.slice(1)) {
    if (message.item.kind === "user_message" && message.item.text)
      out.push("", `Recorded instruction at ${date(message.at)}:`, "", quote(message.item.text));
  }
  if (messages.length < 2) out.push("", "No later decisions or instructions are present in the loaded history.");
  for (const approval of approvals.filter((a) => a.task_id === task.task_id)
    .sort((a, b) => a.requested_at - b.requested_at || a.approval_id.localeCompare(b.approval_id))) {
    out.push("", `Permission request: ${line(approval.title)}. Scope: ${line(approval.scope)}.`,
      `Recorded decision: ${approval.state.replaceAll("_", " ")}${approval.outcome ? `; agent reported ${approval.outcome}` : ""}.`);
  }
  out.push("", "## Annotation references", "");
  const seen = new Set<string>();
  let annotations = 0;
  for (const message of messages) {
    if (message.item.kind !== "user_message" || !message.item.bundle) continue;
    const bundle = message.item.bundle;
    const key = `${bundle.bundle_id}/${bundle.revision}`;
    if (seen.has(key)) continue;
    seen.add(key); annotations++;
    out.push(`### Bundle ${bundle.bundle_id}, revision ${bundle.revision}`, "");
    if (bundle.general_note) out.push(quote(bundle.general_note), "");
    for (const comment of bundle.comments.filter((c) => c.mark_ids.length === 0)) out.push(quote(comment.text), "");
    for (const step of [...bundle.steps].sort((a, b) => a.index - b.index))
      out.push(`- Step ${step.index}, source ${line(step.source_key)}${step.clip_range_ms ? `, ${step.clip_range_ms[0]}–${step.clip_range_ms[1]} ms` : ""}: ${line(step.text)}`);
    for (const source of bundle.sources)
      out.push(`- Source ${line(source.key)}: artifact ${source.artifact_id}; SHA-256 ${source.sha256}; captured ${date(source.captured_at)}.`);
    for (const mark of bundle.marks) {
      out.push(`- Drawing ${line(mark.label)}: ${mark.mark_id}; source ${line(mark.source_key)}${mark.deleted ? "; removed" : ""}.`);
      for (const comment of bundle.comments.filter((c) => c.mark_ids.includes(mark.mark_id)))
        out.push("", quote(comment.text));
      const after = artifacts.filter((a) => a.links?.some((l) => l.task_id === task.task_id && l.role === "after" &&
        l.annotation?.bundle_id === bundle.bundle_id && l.annotation.revision === bundle.revision &&
        l.annotation.mark_ids.includes(mark.mark_id))).sort((a, b) => a.artifact_id.localeCompare(b.artifact_id));
      for (const a of after) out.push(`  After-evidence: artifact ${a.artifact_id}; SHA-256 ${a.sha256}. Visual review required.`);
    }
    out.push("", "Artifact references are retained on the source computer; this document does not embed their bytes.", "");
  }
  if (!annotations) out.push("No annotation references are present in the loaded history.");
  out.push("", "## Requested completion checks", "");
  out.push(...(task.mission?.completion_checks.length ? task.mission.completion_checks.map((c) => `- ${line(c)}`) : ["None recorded."]));
  const orderedRuns = runs.filter((r) => r.task_id === task.task_id)
    .sort((a, b) => a.admitted_at - b.admitted_at || a.run_id.localeCompare(b.run_id));
  out.push("", "## Revisions, outcomes and checks", "");
  for (const run of orderedRuns) {
    out.push(`### Run ${run.run_id}`, "", `Started: ${date(run.admitted_at)}. Ended: ${run.ended_at === null ? "not recorded" : date(run.ended_at)}.`);
    for (const [label, revision] of [["Started from", run.revisions?.before], ["Ended at", run.revisions?.after]] as const)
      out.push(`${label}: ${revision ? `${revision.commit}; observed ${date(revision.observed_at)}; ${revision.dirty === true ? "uncommitted changes present" : revision.dirty === false ? "working tree clean" : "working tree state unknown"}` : "repository revision not recorded"}.`);
    out.push("", "Agent summary:", "", run.outcome?.summary ? quote(run.outcome.summary) : "No summary recorded.", "");
    const stop = stopReceipt(history, run.run_id);
    if (stop) {
      out.push("Stop receipt:", "", `${line(stop.summary)}`);
      const progress = stopProgress(history, run.run_id);
      if (progress?.said) out.push(`The agent’s last message before Stop: “${progress.said}”`);
      if (progress?.last) out.push(`${progress.finished === 1 ? "1 step" : `${progress.finished} steps`} finished before Stop${progress.failed ? `, ${progress.failed} failed` : ""}. Last step: ${line(progress.last.name)}${progress.last.input ? ` ${line(progress.last.input)}` : ""} (${progress.last.ended}).`);
      out.push("");
      for (const fact of stop.facts) out.push(`- ${line(fact.label)}: ${stopFactWord(fact)}${fact.detail ? `. ${line(fact.detail)}` : "."}`);
      out.push("");
    }
    for (const file of run.outcome?.changed_files ?? [])
      out.push(`- Changed file: ${line(file.path)} (${file.change}; ${file.provenance === "native" ? "observed from the agent’s tools" : "source not recorded"}).`);
    out.push("", "Checks:", "");
    if (!run.outcome?.checks.length) out.push("No check results recorded.");
    for (const check of run.outcome?.checks ?? [])
      out.push(`- ${line(check.command ?? check.name)}: ${check.status.replaceAll("_", " ")}; ${check.provenance === "observed" ? "run by the computer" : check.provenance === "native" ? "reported by the agent" : "inferred, not confirmed"}${check.exit_code != null ? `; exit status ${check.exit_code}` : "; exit status not recorded"}.`);
  }
  if (!orderedRuns.length) out.push("No run records are available.");
  out.push("", "## Open gaps", "");
  if (!historyComplete) out.push("- Earlier conversation history may be missing from this brief.");
  let gaps = historyComplete ? 0 : 1;
  if (!evidenceComplete) { out.push("- Task pictures and files have not all been loaded."); gaps++; }
  for (const run of orderedRuns) {
    for (const gap of [...(run.outcome?.unknowns ?? []), ...(run.outcome?.still_active ?? []),
      ...run.children.filter((c) => c.state === "active" || c.state === "unknown").map((c) => `${c.label}: ${c.state === "active" ? "still running" : "state unknown"}`)]) {
      out.push(`- ${line(gap)} (run ${run.run_id})`); gaps++;
    }
    for (const fact of stopUnknowns(stopReceipt(history, run.run_id))
      .filter((f) => !run.children.some((c) => (c.state === "active" || c.state === "unknown") && f.label === `Child: ${c.label}`))) {
      out.push(`- After Stop, ${line(fact.label.replace(/^Child: /, ""))}: ${stopFactWord(fact).toLowerCase()}${fact.detail ? `. ${line(fact.detail)}` : ""} (run ${run.run_id})`); gaps++;
    }
    if (!run.outcome) { out.push(`- No complete outcome for run ${run.run_id}.`); gaps++; }
  }
  for (const requested of task.mission?.completion_checks ?? []) {
    const check = orderedRuns.at(-1)?.outcome?.checks.findLast((c) => c.command === requested && c.provenance !== "inferred");
    if (check?.status !== "passed") {
      out.push(`- Requested check ${line(requested)}: ${check ? check.status.replaceAll("_", " ") : "the exact command was not observed"}.`); gaps++;
    }
  }
  if (!gaps) out.push("- No open gaps were reported. This does not independently verify completion.");
  out.push("", "## Source record", "", `Source task: ${task.task_id}`,
    `Project: ${line(project?.name ?? "Project name not available")}`,
    `Recorded state: ${recordedState(task, orderedRuns.find((r) => r.run_id === task.current_run))}`, `Last task update: ${date(task.updated_at)}`);
  return out.join("\n").trimEnd() + "\n";
}
/** A person's Mark completed or failed is their decision, never the computer's finding. */
function recordedState(task: Task, current: Run | undefined): string {
  const decided = decisionLabel(current);
  return decided ? `${decided} by a person; not verified by the computer` : planProposed(task) ? "Plan proposed" : taskWords[task.state];
}
export function eligibleBriefAgents(harnesses: HarnessInfo[]): HarnessInfo[] {
  return harnesses.filter((h) => h.integration === "managed" && h.installed.state === "available" &&
    h.account.state === "signed_in" && h.capabilities.start_run.state === "available" &&
    (!h.modes?.length || h.modes.some((m) => m.mode === "work" && m.availability.state === "available")));
}
