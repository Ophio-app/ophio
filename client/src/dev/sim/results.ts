// SPDX-License-Identifier: Apache-2.0
import type { AnnotationBundle, ArtifactMeta, Run, Task } from "../../protocol/types";
import type { SimHost } from "./host";

/** Native-shaped receipt evidence used only in development. */
export function resultFixture(tasks: Task[], run: Run): void {
  const task = tasks.find((t) => t.task_id === run.task_id);
  if (task) task.mission = {
    goal: "Refine the checkout page",
    bounds: [{ text: "Keep the checkout button color", enforcement: "requested_only" }],
    completion_checks: ["npm run test:layout -- heading", "npm run test:keyboard", "npm run test:mobile"],
    stop_point: "Return the changes and any checks that could not finish.",
  };
  if (!run.outcome) return;
  run.outcome.changed_files = run.outcome.changed_files.map((file, i) => ({
    ...file, provenance: "native",
    ...(i === 1 ? { diff: { text: `--- a/src/checkout.css
+++ b/src/checkout.css
@@ -12,3 +12,3 @@
 .checkout-heading {
-  margin-bottom: 12px;
+  margin-bottom: 24px;
 }`, truncated: false } } : {}),
  }));
  run.outcome.checks = run.outcome.checks.map((check) => ({
    ...check, provenance: "native", exit_code: check.status === "passed" ? 0 : check.status === "failed" ? 1 : undefined,
    output: check.status === "passed" ? `${check.name}: passed\n` : check.status === "failed" ? `${check.name}: failed\n` : `${check.name}: no result\n`,
    after_last_change: true,
  }));
  const latest = new Map(run.outcome.checks.filter((check) => check.status !== "not_run").map((check) => [check.command ?? check.name, check]));
  const checks = [...latest.values()];
  const passed = checks.filter((check) => check.status === "passed").length;
  const failed = checks.filter((check) => check.status === "failed").length;
  const unknown = checks.length - passed - failed;
  run.outcome.verification = { status: failed ? "failed" : unknown ? "unknown" : passed ? "verified" : "not_checked", passed, failed, unknown };
  run.revisions = {
    before: { commit: "a".repeat(40), observed_at: run.admitted_at, dirty: true },
    after: { commit: "a".repeat(40), observed_at: run.ended_at ?? run.admitted_at, dirty: true },
  };
}
export function linkReceiptPictures(host: SimHost, run: Run, bundle: AnnotationBundle, original: ArtifactMeta, heading: ArtifactMeta, button: ArtifactMeta): void {
  for (const [artifact, role, marks] of [
    [original, "source", bundle.marks.map((m) => m.mark_id)],
    [heading, "after", bundle.marks.slice(0, 1).map((m) => m.mark_id)],
    [button, "after", bundle.marks.slice(1, 2).map((m) => m.mark_id)],
  ] as const) host.emit({ type: "artifact_created", artifact: {
    ...artifact, task_id: run.task_id, links: [{
      task_id: run.task_id, run_id: run.run_id, role,
      annotation: { bundle_id: bundle.bundle_id, revision: bundle.revision, mark_ids: [...marks] },
    }],
  } });
}
