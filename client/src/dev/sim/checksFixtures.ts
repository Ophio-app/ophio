// SPDX-License-Identifier: Apache-2.0
import type { Check } from "../../protocol/generated/Check";
import type { CheckJob } from "../../protocol/generated/CheckJob";
import type { ChecksDraft } from "../../protocol/generated/ChecksDraft";
import type { OphioCheckResult } from "../../protocol/generated/OphioCheckResult";
import type { ProjectChecks } from "../../protocol/generated/ProjectChecks";
import type { SimHost } from "./host";
import { ids } from "./scenario";

export const checksIds = {
  unit: "0192f7a1-0000-7000-8000-00000000e001",
  types: "0192f7a1-0000-7000-8000-00000000e002",
  build: "0192f7a1-0000-7000-8000-00000000e003",
  links: "0192f7a1-0000-7000-8000-00000000e004",
  added: "0192f7a1-0000-7000-8000-00000000e005",
  draft: "0192f7a1-0000-7000-8000-00000000e010",
  stale: "0192f7a1-0000-7000-8000-00000000e011",
  receipt: "0192f7a1-0000-7000-8000-00000000e020",
  checkingJob: "0192f7a1-0000-7000-8000-00000000e021",
  checkingTask: "0192f7a1-0000-7000-8000-00000000e030",
  checkingRun: "0192f7a1-0000-7000-8000-00000000e031",
};

export function sampleChecks(): Check[] {
  const make = (check_id: string, name: string, about: string, run: string, extra: Partial<Check> = {}): Check => ({
    check_id, name, about, run, setup: null, paths: [], when: "every_task", timeout_secs: 600,
    network: false, inputs: [], enabled: true, typical_secs: 72, ...extra,
  });
  return [
    make(checksIds.unit, "Unit tests", "Checks that the app’s individual parts still work. A failure means something behaves differently than expected.", "npm test -- --run", { paths: ["src/**", "tests/**"], inputs: ["package.json"], typical_secs: 72 }),
    make(checksIds.types, "Code consistency", "Checks that the app’s parts fit together. A failure means a change needs fixing before it can be built.", "npm run typecheck", { typical_secs: 18 }),
    make(checksIds.build, "Build the app", "Makes a complete copy of the app. A failure means this version cannot be packaged yet.", "npm run build", { typical_secs: 125 }),
    make(checksIds.links, "Website links", "Checks that the links on the website still open. A failure means a page may be missing or unavailable.", "npm run check-links", { network: true, timeout_secs: 300, typical_secs: 36 }),
  ];
}

/** These are simulated observations, never an execution of a fixture command. */
export function sampleResult(check: Check, inputsChanged = false): OphioCheckResult {
  const failed = check.check_id === checksIds.unit || /\bfail\b/.test(check.run);
  return { check_id: check.check_id, name: check.name, outcome: !check.enabled ? { kind: "off" } : { kind: failed ? "failed" : "passed" },
    duration_secs: check.enabled ? check.typical_secs ?? 12 : 0, exit_code: !check.enabled ? null : failed ? 1 : 0,
    output: !check.enabled ? "" : failed ? "Simulated check output\nFAIL tests/orders.test.ts\nExpected Retry-After to be 60, received 0.\n123 passed, 1 failed."
      : "Simulated check output\nAll checks passed.\nFinished successfully.",
    inputs_changed: inputsChanged ? [...check.inputs] : [], };
}

export function emptyChecks(projectId: string, now = Date.now()): ProjectChecks {
  return { project_id: projectId, version: 0, checks: [], input_fingerprints: [], changed_at: now, changed_by: { kind: "person", device_id: null } };
}

export function checksFixtures(host: SimHost, now: number, checkingTask = false) {
  const approved = { ...emptyChecks(ids.pTide, now - 2 * 86400_000), version: 2, checks: sampleChecks(),
    input_fingerprints: [{ check_id: checksIds.unit, path: "package.json", sha256: "a".repeat(64) }] };
  const garden: ProjectChecks = { ...emptyChecks(ids.pGarden, now - 86400_000), version: 1, checks: sampleChecks(),
    input_fingerprints: [{ check_id: checksIds.unit, path: "package.json", sha256: null }] };
  const added: Check = { ...sampleChecks()[2]!, check_id: checksIds.added, name: "Android build", about: "Checks that the phone app can still be built for Android. A failure means the phone package needs fixing.", run: "npm run build:android", typical_secs: null };
  const draft: ChecksDraft = { id: checksIds.draft, project_id: ids.pTide, base_version: 2, task_id: ids.tHeaders,
    request: "Add an Android build check and simplify the existing checks", state: "ready", created_at: now - 50 * 60_000, updated_at: now - 48 * 60_000,
    items: [{ kind: "add", check: added, note: "Also check the Android package." },
      { kind: "change", check: { ...approved.checks[0]!, network: true, timeout_secs: 900, run: "npm run test:all" }, note: "Include the tests that fetch sample data." },
      { kind: "remove", check_id: checksIds.links, reason: "The new build already checks the website links." }],
    proofs: [sampleResult(added), sampleResult({ ...approved.checks[0]!, network: true })] };
  const stale: ChecksDraft = { ...structuredClone(draft), id: checksIds.stale, project_id: ids.pGarden, base_version: 0, task_id: null, state: "stale", request: "Check that the garden app still builds" };
  const receipt: CheckJob = { id: checksIds.receipt, project_id: ids.pTide, task_id: ids.tHeaders, run_id: ids.rHeaders,
    purpose: { kind: "after_run" }, checks_version: 2, checks: structuredClone(approved.checks), state: "finished", process: null,
    created_at: now - 55 * 60_000, started_at: now - 55 * 60_000, finished_at: now - 52 * 60_000,
    sandbox: host.variant === "windows" ? { kind: "unsandboxed", reason: "This computer cannot isolate checks yet." } : { kind: "contained" },
    results: approved.checks.map(check => sampleResult(check, check.check_id === checksIds.unit)), message: null };
  const checking: CheckJob = { ...structuredClone(receipt), id: checksIds.checkingJob, project_id: ids.pGarden, task_id: checksIds.checkingTask,
    run_id: checksIds.checkingRun, checks_version: 1, state: "running", created_at: now - 20_000, started_at: now - 15_000, finished_at: null,
    results: garden.checks.slice(0, 2).map(check => sampleResult(check)), process: { pid: 4172, start_time: "sim-process-4172", sandbox_unit: "sim-checks" } };
  // Keep the ordinary simulator's two active tasks stable for existing drills.
  // The checks demo opts into a third task with its own occupied project.
  if (!checkingTask) return { approved: [approved, garden], drafts: [draft, stale], jobs: [receipt] };
  const template = host.tasks.find(task => task.task_id === ids.tHeaders)!;
  host.tasks.push({ ...structuredClone(template), task_id: checksIds.checkingTask, project_id: ids.pGarden, mode: "work", conversation: false, github: undefined, review: undefined, report: undefined, wake: undefined, asked_by: undefined, title: "Fix the garden app’s login screen",
    state: "working", current_run: checksIds.checkingRun, revision: 1, created_at: now - 120_000, updated_at: now - 15_000,
    status_line: "Checking…", checking: { job_id: checking.id, started_at: checking.started_at!, done: 2, total: 4 }, attention_count: 0 });
  const run = host.runs.find(run => run.run_id === ids.rHeaders)!;
  host.runs.push({ ...structuredClone(run), run_id: checksIds.checkingRun, task_id: checksIds.checkingTask, state: "completed", holds_writer: true, cleanup_pending: true,
    admitted_at: now - 120_000, dispatched_at: now - 119_000, acknowledged_at: now - 118_000, ended_at: now - 20_000, outcome: null });
  const project = host.projects.find(project => project.project_id === ids.pGarden);
  if (project) project.active_writer = checksIds.checkingRun;
  return { approved: [approved, garden], drafts: [draft, stale], jobs: [receipt, checking] };
}
