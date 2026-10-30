// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import { buildBrief, digestSentences, stopChecks } from "./brief";
import {
  modelLabel,
  dependencyError,
  hostPathError,
  latestRun,
  libraryLocation,
  markEvidence,
  moveError,
  receiptHeadline,
  runDoubts,
  verificationLine,
  followUpLine,
} from "./logic";
import { projects, tasks, runs, queue } from "../../dev/sim/scenario";
import { emptyHostState } from "../../state/projection";
import type {
  AnnotationBundle,
  ArtifactMeta,
  AttentionItem,
  QueueItem,
} from "../../protocol/types";
const now = 100_000_000;
const task = { ...tasks(now)[1]!, current_run: null };
const run = {
  ...runs(now)[1]!,
  children: [
    {
      child_id: "helper",
      label: "Review",
      state: "active" as const,
      native_ref: null,
      started_at: now - 60000,
      ended_at: null,
      provenance: "native" as const,
    },
  ],
};
const bundle: AnnotationBundle = {
  bundle_id: "bundle",
  revision: 1,
  sources: [
    {
      key: "S1",
      artifact_id: "before",
      sha256: "hash",
      media_type: "image/png",
      width: 390,
      height: 200,
      origin: { kind: "phone_import", original_name: "before.png" },
      captured_at: 1,
      derived_from: null,
      derivation: null,
    },
  ],
  marks: ["A", "B", "C"].map((label) => ({
    mark_id: label,
    label,
    source_key: "S1",
    shape: { kind: "pin", at: { x: 0.5, y: 0.5, t: null } },
    color: 0,
    created_at: 2,
    deleted: false,
  })),
  comments: [
    {
      comment_id: "comment",
      text: "Keep the color",
      mark_ids: ["B"],
      origin: { kind: "typed" },
      link_origins: ["manual"],
    },
  ],
  steps: [],
  general_note: null,
  speech: null,
};
const artifact = (
  label: string,
  patch: Partial<ArtifactMeta> = {},
): ArtifactMeta => ({
  artifact_id: label,
  media_type: "image/png",
  size: 10,
  sha256: "hash",
  created_at: now,
  task_id: task.task_id,
  label,
  ...patch,
});
describe("return brief", () => {
  it("is deterministic, interval-bound and preserves incomplete children and provenance", () => {
    const result = buildBrief(task.project_id, 0, [task], [run], []);
    expect(result).toEqual(buildBrief(task.project_id, 0, [task], [run], []));
    expect(result.finished).toEqual([task.task_id]);
    expect(result.changedFiles).toBe(3);
    expect(result.checks.passed).toBe(0);
    expect(result.checks.reported).toBe(2);
    expect(result.checks.inferred).toBe(1);
    expect(result.unknowns).toContain(`Review from “${task.title}” is still running.`);
    expect(result.unknowns).toContain(
      `“${task.title}”: Whether the gateway strips unknown headers in production`,
    );
    expect(
      buildBrief(task.project_id, run.ended_at ?? now, [task], [run], [])
        .finished,
    ).toEqual([]);
    expect(
      buildBrief("another-project", 0, [task], [run], []).changedFiles,
    ).toBe(0);
  });
  it("does not infer completion or checks when a run has no result", () => {
    const result = buildBrief(
      task.project_id,
      0,
      [task],
      [{ ...run, state: "reconciliation_required", outcome: null }],
      [],
    );
    expect(result.finished).toEqual([]);
    expect(result.unknowns).toContain(`The result of “${task.title}” hasn’t been reported.`);
  });
  it("words a stopped run's missing result as the Stop, not as an unreported update", () => {
    const brief = buildBrief(task.project_id, 0, [task], [{ ...run, state: "interrupted", outcome: null, children: [] }], []);
    expect(brief.unknowns).toEqual([`“${task.title}” was stopped before it reported a result.`]);
    expect(digestSentences(brief, 0).lead).toBe(`“${task.title}” was stopped before it reported a result.`);
  });
  it("words the check a Stop leaves as that check, not as a task update", () => {
    const check: AttentionItem = { attention_id: "a1", kind: "reconciliation_required", title: task.title, body: null, task_id: task.task_id,
      run_id: run.run_id, approval_id: null, created_at: now, resolved_at: null, priority: 5 };
    const receipt = { kind: "stop_task" as const, requested_at: now, facts: [], summary: "The agent confirmed Stop." };
    const state = { ...emptyHostState, tasks: { [task.task_id]: task },
      timelines: { [task.task_id]: { hasMore: false, loaded: true, items: [{ seq: 1, at: now, run_id: run.run_id, item: { kind: "receipt" as const, receipt } }] } } };
    const brief = buildBrief(task.project_id, 0, [task], [{ ...run, state: "reconciliation_required" }], [check]);
    expect(digestSentences(brief, 1, stopChecks(state, [check])).lead).toBe(`Check that “${task.title}” stopped.`);
    // Beside another update, the count leads and the check comes first after it.
    const mixed = digestSentences(brief, 2, stopChecks(state, [check]));
    expect(mixed.lead).toBe("2 task updates need attention.");
    expect(mixed.details[0]).toBe(`Check that “${task.title}” stopped.`);
    // A reconciliation no Stop of this run ended stays a task update.
    const other = { ...state, timelines: { [task.task_id]: { ...state.timelines[task.task_id]!, items: [{ ...state.timelines[task.task_id]!.items[0]!, run_id: "earlier-run" }] } } };
    expect(stopChecks(other, [check])).toEqual([]);
    expect(digestSentences(brief, 1, stopChecks(other, [check])).lead).toBe("1 task update needs attention.");
  });
});
describe("receipt truth", () => {
  it("never calls a parent all done while children or unknowns remain", () => {
    expect(receiptHeadline(task, run)).toContain("1 helper still running");
    expect(receiptHeadline(task, run)).toContain("unknowns remain");
    expect(receiptHeadline(task, run)).not.toContain("Done");
    expect(
      receiptHeadline(task, {
        ...run,
        children: [{ ...run.children[0]!, state: "unknown" }],
        outcome: null,
      }),
    ).toContain("unknowns remain");
    expect(receiptHeadline(task)).toContain("not reported");
  });
  it("finds a completed run when current_run is null", () =>
    expect(latestRun(task, { [run.run_id]: run })).toBe(run));
  it("preserves A/B/C evidence and ignores labels and other annotation revisions", () => {
    const result = markEvidence(
      bundle,
      {
        ...run,
        outcome: {
          ...run.outcome!,
          checks: [
            {
              name: "B: Color contrast",
              command: null,
              status: "unknown",
              detail: null,
              provenance: "native",
            },
          ],
        },
      },
      [
        artifact("Unrelated label", { links: [{ task_id: run.task_id, run_id: run.run_id, role: "after", annotation: { bundle_id: bundle.bundle_id, revision: bundle.revision, mark_ids: ["A", "B"] } }] }),
        artifact("A — label only"),
        artifact("C — wrong revision", { links: [{ task_id: run.task_id, run_id: run.run_id, role: "after", annotation: { bundle_id: bundle.bundle_id, revision: 2, mark_ids: ["C"] } }] }),
        artifact("C — other task", { task_id: "another" }),
      ],
    );
    expect(result.map((r) => r.status)).toEqual([
      "Evidence found",
      "Evidence found",
      "No after picture",
    ]);
    expect(result[1]?.comments[0]?.text).toBe("Keep the color");
    expect(result[2]?.reason).toBe("No after picture linked to this drawing");
    expect(result[0]?.reason).toContain("Inspect");
  });
});
describe("queue edits", () => {
  const base = queue(now).items[0]!;
  const items: QueueItem[] = [
    { ...base, item_id: "a", position: 0 },
    { ...base, item_id: "b", position: 1, depends_on: ["a"] },
    { ...base, item_id: "c", position: 2, depends_on: ["b"] },
  ];
  it("refuses transitive cycles, self-links and disappeared items", () => {
    expect(dependencyError(items, "a", ["c"])).toContain("cycle");
    expect(dependencyError(items, "a", ["a"])).toContain("itself");
    expect(dependencyError(items, "a", ["missing"])).toContain("left");
    expect(dependencyError(items, "c", ["a"])).toBeNull();
  });
  it("bounds moves and protects started positions", () => {
    expect(moveError(items, "b", 0)).toBeNull();
    expect(moveError(items, "b", -1)).not.toBeNull();
    expect(
      moveError(
        [
          { ...items[0]!, state: { state: "started", run_id: "run" } },
          ...items.slice(1),
        ],
        "b",
        0,
      ),
    ).toContain("started");
  });
});
describe("host paths", () => {
  it.each(["/home/sam/Projects/app", "/srv/apps", "/"])(
    "accepts POSIX %s",
    (p) => expect(hostPathError(p, "Linux")).toBeNull(),
  );
  it.each(["C:\\Projects\\app", "D:/Work/app", "\\\\server\\share\\app"])(
    "accepts Windows %s",
    (p) => expect(hostPathError(p, "Windows 11")).toBeNull(),
  );
  it.each(["work/app", "~/app", "/home/sam/../secret", "\u0000bad"])(
    "refuses POSIX %s",
    (p) => expect(hostPathError(p, "Linux")).not.toBeNull(),
  );
  it.each([
    "C:relative",
    "\\rooted",
    "C:\\app\\..\\secret",
    "C:\\app*",
    "\\\\?\\C:\\app",
  ])("refuses Windows %s", (p) =>
    expect(hostPathError(p, "Windows")).not.toBeNull(),
  );
  it("does not guess an unknown OS", () =>
    expect(hostPathError("/app", null)).not.toBeNull());
  it("only opens changed files within an authorized root", () => {
    const project = projects(now)[0]!;
    const root = {
      root_id: "root",
      label: "Project",
      host_path: project.host_path,
      kind: "project" as const,
      writable: true,
      project_id: project.project_id,
      harness: null,
    };
    expect(libraryLocation("README.md", project, [root])).toEqual({
      rootId: "root",
      path: "README.md",
    });
    expect(libraryLocation("../../secret", project, [root])).toBeNull();
    expect(
      libraryLocation(`${project.host_path}-other/file`, project, [root]),
    ).toBeNull();
  });
});

it("describes queued model requests without implying missing run reporting", () => {
  const queued = { ...task, state: "accepted" as const, model: "claude-sonnet-5", effort: "medium" };
  expect(modelLabel(undefined, [], queued)).toBe("Requested Claude Sonnet 5 · Medium");
  expect(modelLabel(undefined, [], { ...queued, model: null })).toBe("Model chosen at start");
  expect(modelLabel(undefined, [], { ...queued, state: "completed" })).toBe("Model not reported");
});

it("finds each task's latest run with one pass over the runs, however many tasks ask", () => {
  const base = runs(1000)[0]!;
  const older = { ...base, run_id: "a-older", admitted_at: 100 };
  const tiedLow = { ...base, run_id: "b-tied", admitted_at: 200 };
  const tiedHigh = { ...base, run_id: "c-tied", admitted_at: 200 };
  const elsewhere = { ...base, run_id: "d-elsewhere", task_id: "another task", admitted_at: 900 };
  let reads = 0;
  const records = new Proxy(Object.fromEntries([tiedHigh, older, elsewhere, tiedLow].map((r) => [r.run_id, r])), {
    ownKeys: (target) => { reads++; return Reflect.ownKeys(target); },
  });
  const owner = { ...task, task_id: base.task_id, current_run: null };
  for (let i = 0; i < 5; i++) expect(latestRun(owner, records)).toBe(tiedHigh);
  expect(latestRun({ ...owner, task_id: "another task" }, records)).toBe(elsewhere);
  expect(latestRun({ ...owner, current_run: "not arrived" }, records)).toBeUndefined();
  expect(latestRun({ ...owner, task_id: "no runs" }, records)).toBeUndefined();
  expect(reads).toBe(1);
});

describe("run verification and follow-up", () => {
  const base = { ...run, children: [], outcome: { ...run.outcome!, changed_files: [{ path: "a" }], checks: [], unknowns: [], still_active: [] } } as unknown as typeof run;
  const verified = (status: "verified" | "failed" | "unknown" | "not_checked", passed = 0, failed = 0, unknown = 0) => ({
    ...base, outcome: { ...base.outcome!, verification: { status, passed, failed, unknown } },
  });
  it("counts only final failed checks, retaining the old-record fallback", () => {
    const oldFailure = { name: "test", command: "npm test", status: "failed" as const, detail: null, provenance: "native" as const };
    const recovered = { ...verified("verified", 1), outcome: { ...verified("verified", 1).outcome!, checks: [oldFailure, { ...oldFailure, status: "passed" as const }] } };
    expect(runDoubts(recovered)).toEqual([]);
    expect(runDoubts({ ...recovered, outcome: { ...recovered.outcome!, verification: undefined } })).toEqual(["1 check failed"]);
    // Failed, changed, never run again: still a doubt.
    const unrun = { ...recovered, outcome: { ...recovered.outcome!, checks: [oldFailure], verification: { status: "not_checked" as const, passed: 0, failed: 0, unknown: 0, failed_not_rerun: 1 } } };
    expect(runDoubts(unrun)).toEqual(["1 check failed and wasn’t run again"]);
  });
  it("words every verification status and omits empty turns and old records", () => {
    expect(verificationLine(verified("verified", 1))).toBe("Checked: the check after its last change passed.");
    expect(verificationLine(verified("verified", 2))).toBe("Checked: all 2 checks after its last change passed.");
    expect(verificationLine(verified("failed", 0, 2))).toBe("2 checks failed after its last change.");
    expect(verificationLine(verified("unknown", 0, 0, 1))).toBe("Not verified: 1 check after its last change has no result of its own.");
    expect(verificationLine(verified("unknown", 0, 0, 2))).toBe("Not verified: 2 checks after its last change have no results of their own.");
    expect(verificationLine(verified("not_checked"))).toBe("Not verified: no check ran after its last change.");
    expect(verificationLine({ ...base, outcome: { ...base.outcome!, verification: { status: "unknown", passed: 1, failed: 0, unknown: 0, failed_not_rerun: 1 } } }))
      .toBe("Not verified: a check that failed earlier wasn’t run again.");
    expect(verificationLine({ ...base, outcome: { ...base.outcome!, verification: { status: "unknown", passed: 1, failed: 0, unknown: 0, incomplete: "A command that may change files went on in the background." } } }))
      .toBe("Not verified. A command that may change files went on in the background.");
    expect(verificationLine({ ...verified("not_checked"), outcome: { ...base.outcome!, changed_files: [], checks: [], verification: { status: "not_checked", passed: 0, failed: 0, unknown: 0 } } })).toBeNull();
    expect(verificationLine(base)).toBeNull();
  });
  it("words both reasons a later run was needed", () => {
    expect(followUpLine(base)).toBeNull();
    expect(followUpLine({ ...base, follow_up: { run_id: "next", at: now, reason: "changed_files" } })).toBe("Needed a follow-up: a later run changed files after this one finished.");
    expect(followUpLine({ ...base, follow_up: { run_id: "next", at: now, reason: "check_failed" } })).toBe("Needed a follow-up: a check failed at the start of the next run.");
  });
});
