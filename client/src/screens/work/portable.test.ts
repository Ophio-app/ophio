// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import { projects, runs, tasks, harnesses } from "../../dev/sim/scenario";
import { portableBrief, eligibleBriefAgents } from "./portable";
import { buildBrief, digestSentences } from "./brief";
import { recentAgentTasks, markEvidence } from "./logic";

const at = 100_000_000;
const task = tasks(at)[1]!;
const run = runs(at)[1]!;
describe("portable task evidence", () => {
  it("produces deterministic Markdown without upgrading agent reports or inventing revisions", () => {
    const input = { task, runs: [run], project: projects(at)[0], timeline: [], artifacts: [], historyComplete: false };
    const result = portableBrief(input);
    expect(result).toBe(portableBrief(input));
    expect(result).toContain("## Goal");
    expect(result).toContain("## Decisions and boundaries");
    expect(result).toContain("## Annotation references");
    expect(result).toContain("Started from: repository revision not recorded");
    expect(result).toContain("reported by the agent");
    expect(result).toContain("Earlier conversation history may be missing");
    expect(result).not.toContain("run by the computer");
    expect(result).toContain("## Open gaps");
  });
  it("excludes unready and observed-only recipients", () => {
    const base = harnesses[0]!;
    expect(eligibleBriefAgents([base])).toHaveLength(1);
    expect(eligibleBriefAgents([{ ...base, integration: "terminal_only" }, { ...base, account: { state: "unknown", reason: "No account observation" } }])).toHaveLength(0);
  });
  it("makes the decision the digest lead and suppresses zero counts", () => {
    const brief = buildBrief(task.project_id, 0, [task], [run], []);
    brief.pendingQuestions = 1;
    const digest = digestSentences(brief);
    expect(digest.lead).toBe("1 request needs your decision.");
    expect(digest.details.join(" ")).not.toMatch(/\b0 /);
    expect(digest.details.join(" ")).toContain("only reported by the agent");
  });
  it("keeps inferred checks separate from native reports in the digest", () => {
    const inferred = { ...run, outcome: { summary: null, changed_files: [], still_active: [], unknowns: [], usage: null,
      checks: [{ name: "Possible test", command: null, status: "passed" as const, detail: null, provenance: "inferred" as const }] } };
    const brief = buildBrief(task.project_id, 0, [task], [inferred], []);
    expect(brief.checks.reported).toBe(0);
    expect(brief.checks.passed).toBe(0);
    expect(digestSentences(brief).details).toContain("1 check was inferred and remains unconfirmed.");
  });
  it("bounds recent agents newest first, leaving unresolved helpers visible separately", () => {
    const a = { ...task, task_id: "a", current_run: "a" };
    const b = { ...task, task_id: "b", current_run: "b" };
    const records = { a: { ...run, run_id: "a", task_id: "a", ended_at: 20, children: [] }, b: { ...run, run_id: "b", task_id: "b", ended_at: 30, children: [] } };
    expect(recentAgentTasks([a, b], records, 1).map((t) => t.task_id)).toEqual(["b"]);
  });
  it("does not find after-evidence by a matching visible label", () => {
    expect(markEvidence({ bundle_id: "bundle", revision: 1, sources: [], marks: [{ mark_id: "m", label: "A", source_key: "s", shape: { kind: "pin", at: { x: 0, y: 0, t: null } }, color: 0, created_at: 0, deleted: false }], comments: [], steps: [], general_note: null, speech: null }, run,
      [{ artifact_id: "a", media_type: "image/png", size: 1, sha256: "hash", created_at: at, label: "A — after", task_id: task.task_id }])[0]?.after).toBeUndefined();
  });
});

it("the Tasks brief counts the same grouped updates as Needs you", () => {
  const brief = buildBrief("project", 0, [], [], []);
  brief.pendingQuestions = 1;
  expect(digestSentences(brief, 4).lead).toBe("4 task updates need attention.");
});
