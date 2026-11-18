// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { CheckJob } from "../../protocol/generated/CheckJob";
import type { Command, Grants } from "../../protocol/types";
import { uuidv7 } from "../../protocol/ids";
import { SimHost } from "./host";
import { ids } from "./scenario";
import { checksIds, sampleChecks } from "./checksFixtures";
import { checkDifferences, checkFacts } from "./checksFacts";

let host: SimHost;
beforeEach(() => { vi.useFakeTimers(); host = new SimHost({ checks: true }); });
afterEach(() => { host.dispose(); vi.useRealTimers(); });
const approved = () => host.checks.approved.get(ids.pTide)!;
const save = (edit: Extract<Command, { command: "checks_save" }>["edit"], base = approved().version) => host.inside({ command: "checks_save", project_id: ids.pTide, base_version: base, edit });
const list = async (project_id: string | null = ids.pTide) => {
  const result = await host.query({ kind: "checks_list", project_id });
  if (result.kind !== "checks_list") throw new Error("Expected checks list");
  return result;
};
const freeTide = () => { host.projects.find(project => project.project_id === ids.pTide)!.active_writer = null; };
const grants: Grants = { view: true, control_input: true, terminal: true, tasks: true, approvals: true, computer_use: true, library_read: true, library_write: true, transfers: true, admin: true, accounts: true };
const command = (body: Command, permissions: Grants = grants) => host.command({ command_id: uuidv7(), issued_at: Date.now(), expected_revision: null, expires_at: null, body }, () => permissions);

it("keeps the extra checking task opt-in so ordinary safety drills retain their fixture counts", () => {
  host.dispose(); host = new SimHost();
  expect(host.tasks.some(task => task.task_id === checksIds.checkingTask)).toBe(false);
  expect(host.checks.jobs.has(checksIds.checkingJob)).toBe(false);
  expect(host.checks.approved.get(ids.pTide)?.checks).toHaveLength(4);
});

it("advertises checks and lists a four-check project, a genuinely empty project and a stale draft", async () => {
  expect(host.host.capabilities.features).toContain("checks_v1");
  const tide = (await list()).projects[0]!;
  expect(tide.checks).toHaveLength(4);
  expect(tide.checks.map(view => view.last_result?.outcome.kind).sort()).toEqual(["failed", "passed", "passed", "passed"]);
  expect(tide.checks[0]).toMatchObject({ inputs_changed: ["package.json"], last_task_id: ids.tHeaders });
  expect(tide.drafts[0]!.draft.items.map(item => item.kind)).toEqual(["add", "change", "remove"]);
  expect((await list(ids.pNotes)).projects[0]).toMatchObject({ approved: { version: 0 }, checks: [], drafts: [] });
  expect((await list(ids.pGarden)).projects[0]!.drafts[0]!.draft.state).toBe("stale");
  const home = await list(null);
  expect(home.projects.some(project => project.project_id === ids.pNotes)).toBe(false);
  expect(home.projects_without_checks).toBeGreaterThanOrEqual(1);
});

it("computes factual lines only from definition fields", () => {
  const check = sampleChecks()[0]!;
  const facts = checkFacts(check);
  expect(facts).toEqual({ when: "After a task changes the app’s code or tests", internet: "Does not use the internet", time_limit: "Time limit: 10 minutes", typical_time: "Takes about 1 minute 12 seconds" });
  expect(checkFacts({ ...check, name: "Uses the internet", about: "Runs only on Fridays" })).toEqual(facts);
  expect(checkFacts({ ...check, paths: [], network: true, timeout_secs: 10, typical_secs: null })).toEqual({ when: "After every task that changes files", internet: "Uses the internet", time_limit: "Time limit: 10 seconds", typical_time: null });
  expect(checkFacts({ ...check, paths: ["docs/**"], when: "before_merge", timeout_secs: 3600 }).when).toContain("not run automatically");
  expect(checkFacts({ ...check, paths: ["docs/**"] }).when).toBe("After a task changes the watched files or folders");
});

it("computes every definition difference without exposing commands or trusting proposal notes", async () => {
  const check = sampleChecks()[0]!;
  expect(checkDifferences(check, { ...check, typical_secs: 999 })).toEqual([]);
  const differences = checkDifferences(check, { ...check, name: "New name", about: "New description", run: "secret-command", setup: "secret-setup", paths: [], when: "before_merge", network: true, timeout_secs: 900, inputs: ["new-input"], enabled: false });
  expect(differences).toHaveLength(10);
  expect(differences).toContain("Now uses the internet");
  expect(differences).toContain("Time limit changes from 10 minutes to 15 minutes");
  expect(differences.join(" ")).not.toMatch(/secret-command|secret-setup|new-input/);
  const draft = (await list()).projects[0]!.drafts[0]!;
  expect(draft.differences).toHaveLength(3);
  expect(draft.facts[2]).toBeNull();
  expect(draft.differences[1]).toContain("Now uses the internet");
});

it("applies on/off and delete immediately, and restores definitions and fingerprints with undo", async () => {
  const before = structuredClone(approved());
  await save({ kind: "set_enabled", check_id: checksIds.unit, enabled: false });
  expect(approved()).toMatchObject({ version: 3, checks: [expect.objectContaining({ enabled: false }), expect.anything(), expect.anything(), expect.anything()] });
  expect(host.checks.drafts.get(checksIds.draft)?.state).toBe("stale");
  await save({ kind: "undo" });
  expect(approved().version).toBe(4);
  expect(approved().checks).toEqual(before.checks);
  expect(approved().input_fingerprints).toEqual(before.input_fingerprints);
  await save({ kind: "delete", check_id: checksIds.unit });
  expect(approved().checks).toHaveLength(3);
  expect(approved().input_fingerprints).toEqual([]);
  await save({ kind: "undo" });
  expect(approved().checks).toEqual(before.checks);
  expect(approved().input_fingerprints).toEqual(before.input_fingerprints);
  const history = await host.query({ kind: "checks_history", project_id: ids.pTide });
  expect(history.kind === "checks_history" && history.versions.map(version => version.version)).toEqual([6, 5, 4, 3, 2, 1]);
});

it("refuses stale saves and permission failures without changing approved data", async () => {
  const before = structuredClone(approved());
  await expect(save({ kind: "delete", check_id: checksIds.unit }, 1)).rejects.toMatchObject({ code: "stale_revision" });
  const body: Command = { command: "checks_save", project_id: ids.pTide, base_version: 2, edit: { kind: "delete", check_id: checksIds.unit } };
  expect(await command(body, { ...grants, library_write: false })).toMatchObject({ status: "rejected", result: null, error: { code: "permission_denied" } });
  host.checks.refuseNext = "This device can no longer change checks.";
  expect(await command(body)).toMatchObject({ status: "rejected", error: { message: "This device can no longer change checks." } });
  expect(approved()).toEqual(before);
  expect(await command(body)).toMatchObject({ status: "accepted", result: { kind: "checks_saved", approved: { version: 3 } } });
});

it("applies only selected draft positions in one approval, with undo", async () => {
  const before = structuredClone(approved());
  const result = await host.inside({ command: "checks_apply_draft", draft_id: checksIds.draft, base_version: 2, keep_items: [0, 2] });
  expect(result).toMatchObject({ kind: "checks_saved", job_id: null, approved: { version: 3, changed_by: { kind: "draft", draft_id: checksIds.draft } } });
  expect(approved().checks.map(check => check.check_id)).toEqual([checksIds.unit, checksIds.types, checksIds.build, checksIds.added]);
  expect(approved().checks[0]).toEqual(before.checks[0]);
  expect(host.checks.drafts.get(checksIds.draft)?.state).toBe("applied");
  await save({ kind: "undo" });
  expect(approved().checks).toEqual(before.checks);
});

it.each([[], [0, 0], [3], [-1], [0.5]].map(keep_items => ({ keep_items })))("refuses malformed draft selection $keep_items without a partial approval", async ({ keep_items }) => {
  const before = structuredClone(approved());
  await expect(host.inside({ command: "checks_apply_draft", draft_id: checksIds.draft, base_version: 2, keep_items })).rejects.toMatchObject({ code: "invalid_request" });
  expect(approved()).toEqual(before);
  expect(host.checks.drafts.get(checksIds.draft)?.state).toBe("ready");
});

it("refuses stale/proving drafts and allows discarding without approving anything", async () => {
  await expect(host.inside({ command: "checks_apply_draft", draft_id: checksIds.stale, base_version: 1, keep_items: [0] })).rejects.toMatchObject({ code: "stale_revision" });
  host.checks.drafts.get(checksIds.draft)!.state = "proving";
  await expect(host.inside({ command: "checks_apply_draft", draft_id: checksIds.draft, base_version: 2, keep_items: [0] })).rejects.toMatchObject({ code: "conflict" });
  expect(await host.inside({ command: "checks_discard_draft", draft_id: checksIds.draft })).toMatchObject({ kind: "checks_draft", draft: { state: "discarded" } });
  expect((await list()).projects[0]!.drafts).toEqual([]);
  expect(approved().version).toBe(2);
});

it("acknowledges only the chosen check’s inputs, and undo restores the notice", async () => {
  const check = approved().checks[1]!;
  check.inputs = ["other.json"];
  approved().input_fingerprints.push({ check_id: check.check_id, path: "other.json", sha256: "c".repeat(64) });
  const oldOther = structuredClone(approved().input_fingerprints[1]);
  await save({ kind: "keep_inputs", check_id: checksIds.unit });
  expect((await list()).projects[0]!.checks[0]!.inputs_changed).toEqual([]);
  expect(approved().input_fingerprints.find(input => input.path === "other.json")).toEqual(oldOther);
  await save({ kind: "undo" });
  expect((await list()).projects[0]!.checks[0]!.inputs_changed).toEqual(["package.json"]);
});

it("refuses Run now while a writer is occupied, then runs a frozen version with visible progress", async () => {
  await expect(host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [] })).rejects.toMatchObject({ code: "conflict" });
  freeTide();
  const result = await host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [] });
  if (result?.kind !== "check_job") throw new Error("Expected check job");
  expect(result.job).toMatchObject({ state: "pending", checks_version: 2, results: [] });
  await expect(host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [] })).rejects.toMatchObject({ code: "conflict" });
  await save({ kind: "set_enabled", check_id: checksIds.unit, enabled: false });
  await vi.advanceTimersByTimeAsync(1100);
  expect(await host.query({ kind: "check_job", job_id: result.job.id })).toMatchObject({ job: { state: "running", results: [expect.objectContaining({ outcome: { kind: "failed" } })] } });
  await vi.advanceTimersByTimeAsync(2500);
  const done = await host.query({ kind: "check_job", job_id: result.job.id });
  expect(done).toMatchObject({ job: { state: "finished", checks_version: 2, checks: [expect.objectContaining({ enabled: true }), expect.anything(), expect.anything(), expect.anything()] } });
  expect(host.projects.find(project => project.project_id === ids.pTide)?.active_writer).toBeNull();
  expect(result.job.state).toBe("pending");
});

it("hand saves approve even a failing check, keep measured duration trustworthy, and run it once", async () => {
  freeTide();
  const result = await save({ kind: "save", check: { ...approved().checks[0]!, run: "npm run fail", typical_secs: 1 } });
  if (result?.kind !== "checks_saved" || !result.job_id) throw new Error("Expected save with a check job");
  expect(result.approved.checks[0]!.typical_secs).toBe(72);
  await vi.advanceTimersByTimeAsync(1100);
  expect(await host.query({ kind: "check_job", job_id: result.job_id })).toMatchObject({ job: { state: "finished", results: [expect.objectContaining({ outcome: { kind: "failed" } })] } });
  expect(approved().checks[0]!.run).toBe("npm run fail");
});

it("a hand save waits for the occupied project, and can be stopped while pending", async () => {
  const saved = await save({ kind: "save", check: approved().checks[0]! });
  if (saved?.kind !== "checks_saved" || !saved.job_id) throw new Error("Expected pending job");
  await vi.advanceTimersByTimeAsync(1000);
  expect(host.checks.jobs.get(saved.job_id)?.state).toBe("pending");
  expect(await host.inside({ command: "checks_stop_job", job_id: saved.job_id })).toMatchObject({ kind: "check_job", job: { state: "finished", message: "Not checked: stopped" } });
  await vi.advanceTimersByTimeAsync(1000);
  expect(host.checks.jobs.get(saved.job_id)?.state).toBe("finished");
  expect(host.projects.find(project => project.project_id === ids.pTide)?.active_writer).toBe(ids.rFlaky);
});

it("stops a run-now job, retaining completed results and marking the remainder stopped", async () => {
  freeTide();
  const result = await host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [] });
  if (result?.kind !== "check_job") throw new Error("Expected job");
  await vi.advanceTimersByTimeAsync(1100);
  expect(await host.inside({ command: "checks_stop_job", job_id: result.job.id })).toMatchObject({ job: { state: "cancelling" } });
  await vi.advanceTimersByTimeAsync(500);
  expect(host.checks.jobs.get(result.job.id)).toMatchObject({ state: "finished", results: [expect.objectContaining({ outcome: { kind: "failed" } }), ...Array.from({ length: 3 }, () => expect.objectContaining({ outcome: { kind: "stopped" } }))] });
  await vi.advanceTimersByTimeAsync(3000);
  expect(host.checks.jobs.get(result.job.id)?.results).toHaveLength(4);
});

it.each([300, 400])("keeps uncertain jobs reserved until an explicit confirmation, and refuses a visible process (start delay: %s)", async (delay) => {
  freeTide();
  host.checks.uncertainStop = true;
  const result = await host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [] });
  if (result?.kind !== "check_job") throw new Error("Expected job");
  await vi.advanceTimersByTimeAsync(delay);
  const stop = { command: "checks_stop_job" as const, job_id: result.job.id };
  await host.inside(stop);
  await vi.advanceTimersByTimeAsync(1000);
  const before = structuredClone(host.checks.jobs.get(result.job.id)!);
  host.checks.uncertainStop = false;
  await host.inside(stop);
  await host.inside({ ...stop, confirmed_stopped: false });
  expect(host.checks.jobs.get(result.job.id)).toEqual(before);
  host.checks.uncertainProcessRunning = true;
  await expect(host.inside({ ...stop, confirmed_stopped: true })).rejects.toMatchObject({ code: "conflict", message: expect.stringContaining("still see the check running") });
  expect(host.checks.jobs.get(result.job.id)).toEqual(before);
  expect(host.projects.find(project => project.project_id === ids.pTide)?.active_writer).toBe(result.job.id);
  host.checks.uncertainProcessRunning = false;
  expect(await host.inside({ ...stop, confirmed_stopped: true })).toMatchObject({ job: { state: "finished", process: null, results: Array.from({ length: 4 }, () => expect.objectContaining({ outcome: { kind: "stopped" } })) } });
  expect(host.projects.find(project => project.project_id === ids.pTide)?.active_writer).toBeNull();
  expect(host.talk.queue(ids.pTide).writer).toBeNull();
});

it("publishes a taskless check job as the project and queue holder until it ends", async () => {
  freeTide();
  const result = await host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [] });
  if (result?.kind !== "check_job") throw new Error("Expected job");
  expect(result.job.task_id).toBeNull();
  expect(host.talk.queue(ids.pTide).writer).toBe(result.job.id);
  expect(host.projects.find(project => project.project_id === ids.pTide)?.active_writer).toBe(result.job.id);
  await host.inside({ command: "checks_stop_job", job_id: result.job.id });
  expect(host.talk.queue(ids.pTide).writer).toBeNull();
  expect(host.projects.find(project => project.project_id === ids.pTide)?.active_writer).toBeNull();
});

it("keeps checking tasks Working for old phones and handles ordinary Stop", async () => {
  const snapshot = await host.query({ kind: "snapshot" });
  if (snapshot.kind !== "snapshot") throw new Error("Expected snapshot");
  expect(snapshot.tasks.find(task => task.task_id === checksIds.checkingTask)).toMatchObject({ state: "working", checking: { done: 2, total: 4 } });
  await expect(host.inside({ command: "checks_stop_job", job_id: checksIds.checkingJob })).rejects.toMatchObject({ code: "invalid_request" });
  await host.inside({ command: "stop_task", task_id: checksIds.checkingTask });
  expect(host.checks.jobs.get(checksIds.checkingJob)?.state).toBe("cancelling");
  expect(host.tasks.find(task => task.task_id === checksIds.checkingTask)?.state).toBe("working");
  expect(host.projects.find(project => project.project_id === ids.pGarden)?.active_writer).toBe(checksIds.checkingRun);
  await vi.advanceTimersByTimeAsync(500);
  expect(host.tasks.find(task => task.task_id === checksIds.checkingTask)).toMatchObject({ state: "completed", status_line: "Not checked: stopped", checking: undefined });
  expect(host.attention.find(item => item.task_id === checksIds.checkingTask)).toMatchObject({ kind: "completed", body: "Not checked: stopped" });
  expect(host.talk.queue(ids.pGarden).paused).toBe(true);
  expect(await host.query({ kind: "task_checks", task_id: checksIds.checkingTask })).toMatchObject({ jobs: [expect.objectContaining({ state: "finished", results: [expect.anything(), expect.anything(), expect.objectContaining({ outcome: { kind: "stopped" } }), expect.objectContaining({ outcome: { kind: "stopped" } })] })] });
});

it("reports an unconfirmed stop and retains the project reservation", async () => {
  host.checks.uncertainStop = true;
  await host.inside({ command: "stop_task", task_id: checksIds.checkingTask });
  expect(host.checks.jobs.get(checksIds.checkingJob)?.state).toBe("cancelling");
  expect(host.tasks.find(task => task.task_id === checksIds.checkingTask)?.state).toBe("working");
  expect(host.projects.find(project => project.project_id === ids.pGarden)?.active_writer).toBe(checksIds.checkingRun);
  await vi.advanceTimersByTimeAsync(500);
  expect(host.tasks.find(task => task.task_id === checksIds.checkingTask)?.state).toBe("reconciliation_required");
  expect(host.checks.jobs.get(checksIds.checkingJob)).toMatchObject({ state: "uncertain", message: expect.stringContaining("could not confirm") });
  expect(host.projects.find(project => project.project_id === ids.pGarden)?.active_writer).toBe(checksIds.checkingRun);
});

it("creates setup/change Work tasks using project defaults and keeps their output as a draft", async () => {
  const setup = await host.inside({ command: "checks_ask", project_id: ids.pNotes, check_id: null, request: "" });
  if (setup?.kind !== "task_created") throw new Error("Expected task");
  expect(host.tasks.find(task => task.task_id === setup.task_id)).toMatchObject({ title: "Set up checks", mode: "work", harness: host.projects.find(project => project.project_id === ids.pNotes)!.default_harness });
  await vi.advanceTimersByTimeAsync(12_000);
  expect((await list(ids.pNotes)).projects[0]).toMatchObject({ approved: { version: 0, checks: [] }, drafts: [expect.objectContaining({ draft: expect.objectContaining({ state: "ready", task_id: setup.task_id }) })] });
  freeTide();
  const change = await host.inside({ command: "checks_ask", project_id: ids.pTide, check_id: checksIds.unit, request: "Allow more time for this check" });
  if (change?.kind !== "task_created") throw new Error("Expected task");
  const timeline = await host.query({ kind: "task_timeline", task_id: change.task_id, before_seq: null, limit: 20 });
  expect(JSON.stringify(timeline)).toContain("Allow more time for this check");
  expect(JSON.stringify(timeline)).toContain("Unit tests");
  await vi.advanceTimersByTimeAsync(12_000);
  const taskChecks = await host.query({ kind: "task_checks", task_id: change.task_id });
  expect(taskChecks).toMatchObject({ drafts: [expect.objectContaining({ draft: expect.objectContaining({ items: [expect.objectContaining({ kind: "change" })], proofs: [expect.anything()] }) })] });
  expect(approved().version).toBe(2);
});

it("queues change tasks behind a writer and requires both permissions", async () => {
  expect(await command({ command: "checks_ask", project_id: ids.pNotes, check_id: null, request: "" }, { ...grants, tasks: false })).toMatchObject({ status: "rejected", error: { code: "permission_denied" } });
  const task = await host.inside({ command: "checks_ask", project_id: ids.pTide, check_id: null, request: "Check the phone build" });
  expect(task).toMatchObject({ kind: "task_created", queue_item_id: expect.any(String) });
  if (task?.kind !== "task_created") throw new Error("Expected queued task");
  expect(host.tasks.find(entry => entry.task_id === task.task_id)).toMatchObject({ state: "accepted", current_run: null });
});

it.each([{ timeout_secs: 9 }, { timeout_secs: 3601 }, { run: " " }, { name: "" }, { inputs: ["../secret"] }, { inputs: ["/secret"] }, { inputs: ["C:\\secret"] }, { paths: ["src/["] }])("refuses invalid hand edits %j", async extra => {
  const before = structuredClone(approved());
  await expect(save({ kind: "save", check: { ...approved().checks[0]!, ...extra } })).rejects.toMatchObject({ code: "invalid_request" });
  expect(approved()).toEqual(before);
});

it("returns plain not-found errors and bounds retained history to twenty versions", async () => {
  await expect(host.query({ kind: "checks_list", project_id: "missing" })).rejects.toMatchObject({ code: "not_found" });
  await expect(host.query({ kind: "task_checks", task_id: "missing" })).rejects.toMatchObject({ code: "not_found" });
  await expect(host.query({ kind: "check_job", job_id: "missing" })).rejects.toMatchObject({ code: "not_found" });
  await expect(host.inside({ command: "checks_discard_draft", draft_id: "missing" })).rejects.toMatchObject({ code: "not_found" });
  await expect(save({ kind: "delete", check_id: "missing" })).rejects.toMatchObject({ code: "not_found" });
  for (let index = 0; index < 25; index++) await save({ kind: "set_enabled", check_id: checksIds.unit, enabled: index % 2 === 0 });
  expect(host.checks.history.get(ids.pTide)).toHaveLength(20);
  expect(host.checks.history.get(ids.pTide)?.[0]?.version).toBe(27);
});

it("keeps jobs pending until their writer releases and runs only enabled checks by default", async () => {
  await save({ kind: "set_enabled", check_id: checksIds.links, enabled: false });
  const saved = await save({ kind: "save", check: approved().checks[1]! });
  if (saved?.kind !== "checks_saved" || !saved.job_id) throw new Error("Expected pending job");
  await vi.advanceTimersByTimeAsync(1000);
  expect(host.checks.jobs.get(saved.job_id)?.state).toBe("pending");
  freeTide();
  await vi.advanceTimersByTimeAsync(1400);
  expect(host.checks.jobs.get(saved.job_id)?.state).toBe("finished");
  const result = await host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [] });
  expect(result).toMatchObject({ kind: "check_job", job: { checks: [expect.anything(), expect.anything(), expect.anything()] } });
});

it("refuses unknown and duplicate explicit run selections", async () => {
  freeTide();
  await expect(host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [checksIds.unit, checksIds.unit] })).rejects.toMatchObject({ code: "invalid_request" });
  await expect(host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: ["missing"] })).rejects.toMatchObject({ code: "not_found" });
  await expect(host.inside({ command: "checks_run_now", project_id: ids.pNotes, check_ids: [] })).rejects.toMatchObject({ code: "invalid_request" });
});

it("the Windows receipt reports its actual lack of sandboxing", async () => {
  host.dispose(); host = new SimHost({ variant: "windows" });
  const taskChecks = await host.query({ kind: "task_checks", task_id: ids.tHeaders });
  expect(taskChecks).toMatchObject({ jobs: [expect.objectContaining({ sandbox: { kind: "unsandboxed", reason: expect.any(String) } })] });
});

it("reports only real approvals in history, including an empty history before first approval", async () => {
  expect(await host.query({ kind: "checks_history", project_id: ids.pNotes })).toEqual({ kind: "checks_history", versions: [] });
  expect(await host.query({ kind: "checks_history", project_id: ids.pGarden })).toMatchObject({ versions: [expect.objectContaining({ version: 1 })] });
  expect(host.checks.history.get(ids.pTide)?.map(version => version.version)).toEqual([2, 1]);
});

it("undoes a first approval to empty definitions and fingerprints without inventing version zero", async () => {
  const result = await host.inside({ command: "checks_save", project_id: ids.pNotes, base_version: 0, edit: { kind: "save", check: sampleChecks()[0]! } });
  expect(result).toMatchObject({ kind: "checks_saved", approved: { version: 1 } });
  expect(await host.query({ kind: "checks_history", project_id: ids.pNotes })).toMatchObject({ versions: [expect.objectContaining({ version: 1 })] });
  expect(host.checks.history.get(ids.pNotes)).toHaveLength(1);
  expect(await host.inside({ command: "checks_save", project_id: ids.pNotes, base_version: 1, edit: { kind: "undo" } })).toMatchObject({ kind: "checks_saved", approved: { version: 2, checks: [], input_fingerprints: [] } });
  expect(host.checks.history.get(ids.pNotes)?.map(version => version.version)).toEqual([2, 1]);
});

it.each([false, true])("keeps a running job cancelling and reserved until termination is checked (uncertain: %s)", async uncertain => {
  freeTide();
  host.checks.uncertainStop = uncertain;
  const started = await host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [] });
  if (started?.kind !== "check_job") throw new Error("Expected job");
  await vi.advanceTimersByTimeAsync(1100);
  const before = structuredClone(host.checks.jobs.get(started.job.id)!);
  const stop = { command: "checks_stop_job" as const, job_id: started.job.id };
  expect(await host.inside(stop)).toMatchObject({ job: { state: "cancelling", finished_at: null, process: before.process, results: before.results } });
  expect(host.projects.find(project => project.project_id === ids.pTide)?.active_writer).toBe(started.job.id);
  expect(host.talk.queue(ids.pTide).writer).toBe(started.job.id);
  await expect(host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [] })).rejects.toMatchObject({ code: "conflict" });
  expect(await host.inside(stop)).toMatchObject({ job: { state: "cancelling" } });
  await vi.advanceTimersByTimeAsync(499);
  expect(host.checks.jobs.get(started.job.id)?.state).toBe("cancelling");
  await vi.advanceTimersByTimeAsync(1);
  const done = host.checks.jobs.get(started.job.id)!;
  expect(done.state).toBe(uncertain ? "uncertain" : "finished");
  expect(done.results[0]).toEqual(before.results[0]);
  expect(done.results.slice(1).map(result => result.outcome.kind)).toEqual(Array(3).fill(uncertain ? "could_not_run" : "stopped"));
  expect(host.talk.queue(ids.pTide).writer).toBe(uncertain ? started.job.id : null);
  expect(done.process).toEqual(uncertain ? before.process : null);
});

it("stops a pending job without a process safely, even when running stops would be uncertain", async () => {
  freeTide();
  host.checks.uncertainStop = true;
  const started = await host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [] });
  if (started?.kind !== "check_job") throw new Error("Expected job");
  expect(await host.inside({ command: "checks_stop_job", job_id: started.job.id })).toMatchObject({ job: { state: "finished", process: null } });
  expect(host.talk.queue(ids.pTide).writer).toBeNull();
  await vi.advanceTimersByTimeAsync(1500);
  expect(host.checks.jobs.get(started.job.id)?.state).toBe("finished");
});

it.each(["run_now", "hand_save"] as const)("executes explicitly selected before-merge checks for %s", async mode => {
  freeTide();
  const check = { ...approved().checks[1]!, when: "before_merge" as const };
  approved().checks[1] = check;
  const result = mode === "run_now"
    ? await host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [check.check_id] })
    : await save({ kind: "save", check });
  const jobId = result?.kind === "check_job" ? result.job.id : result?.kind === "checks_saved" ? result.job_id : null;
  if (!jobId) throw new Error("Expected job");
  await vi.advanceTimersByTimeAsync(1100);
  expect(host.checks.jobs.get(jobId)).toMatchObject({ state: "finished", results: [expect.objectContaining({ check_id: check.check_id, outcome: { kind: "passed" }, exit_code: 0 })] });
});

it("excludes before-merge and disabled checks from an empty automatic selection", async () => {
  freeTide();
  approved().checks[1]!.when = "before_merge";
  approved().checks[2]!.enabled = false;
  expect(await host.inside({ command: "checks_run_now", project_id: ids.pTide, check_ids: [] })).toMatchObject({ job: { checks: [expect.objectContaining({ check_id: checksIds.unit }), expect.objectContaining({ check_id: checksIds.links })] } });
});

it("exposes an uncertain standalone proof job without replacing ordinary result history", async () => {
  const job = structuredClone(host.checks.jobs.get(checksIds.receipt)!);
  Object.assign(job, { id: uuidv7(), task_id: null, run_id: null, state: "uncertain", purpose: { kind: "draft_proof", draft_id: checksIds.draft }, created_at: Date.now() + 1000, finished_at: Date.now() + 2000 });
  job.results = job.results.map(result => ({ ...result, output: "Proof output only", outcome: { kind: "passed" } }));
  host.checks.jobs.set(job.id, job);
  freeTide();
  const project = (await list()).projects[0]!;
  expect(project).toMatchObject({ unfinished_job: job });
  expect(project.checks[0]!.last_result?.outcome.kind).toBe("failed");
  expect(project.checks[0]!.last_result?.output).not.toBe("Proof output only");
  job.state = "finished";
  expect((await list()).projects[0]).not.toHaveProperty("unfinished_job");
});

it.each(["job", "run", "none"] as const)("prefers the reserved unfinished job, otherwise the newest (owner: %s)", async owner => {
  const older = structuredClone(host.checks.jobs.get(checksIds.receipt)!);
  Object.assign(older, { id: uuidv7(), run_id: uuidv7(), state: "running", created_at: Date.now(), finished_at: null });
  const newer: CheckJob = { ...structuredClone(older), id: uuidv7(), run_id: null, state: "pending" as const, created_at: Date.now() + 1000 };
  host.checks.jobs.set(older.id, older);
  host.checks.jobs.set(newer.id, newer);
  host.projects.find(project => project.project_id === ids.pTide)!.active_writer = owner === "job" ? older.id : owner === "run" ? older.run_id : null;
  expect((await list()).projects[0]).toMatchObject({ unfinished_job: owner === "none" ? newer : older });
  newer.state = "cancelling";
  host.checks.jobs.delete(older.id);
  expect((await list()).projects[0]).toMatchObject({ unfinished_job: { id: newer.id, state: "cancelling" } });
});
