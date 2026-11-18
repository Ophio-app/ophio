// SPDX-License-Identifier: Apache-2.0
import type { AttentionItem, Command, CommandResult, Grants, ProtocolError, Query, QueryData } from "../../protocol/types";
import type { Check } from "../../protocol/generated/Check";
import type { CheckJob } from "../../protocol/generated/CheckJob";
import type { ChecksDraft } from "../../protocol/generated/ChecksDraft";
import type { ChecksDraftView } from "../../protocol/generated/ChecksDraftView";
import type { ProjectChecks } from "../../protocol/generated/ProjectChecks";
import type { ProjectChecksView } from "../../protocol/generated/ProjectChecksView";
import { uuidv7 } from "../../protocol/ids";
import type { SimHost } from "./host";
import { ids } from "./scenario";
import { checkDifferences, checkFacts } from "./checksFacts";
import { checksFixtures, emptyChecks, sampleChecks, sampleResult } from "./checksFixtures";

type ChecksCommand = Extract<Command, { command: "checks_save" | "checks_ask" | "checks_apply_draft" | "checks_discard_draft" | "checks_run_now" | "checks_stop_job" }>;
type ChecksQuery = Extract<Query, { kind: "checks_list" | "checks_history" | "task_checks" | "check_job" }>;
const open = (draft: ChecksDraft) => ["proving", "ready", "stale"].includes(draft.state);
const active = (job: CheckJob) => ["pending", "running", "cancelling"].includes(job.state);
const error = (code: ProtocolError["code"], message: string): ProtocolError => ({ code, message });
const stale = () => error("stale_revision", "These checks changed since you opened them. Open them again before saving.");

export class ChecksSim {
  readonly approved = new Map<string, ProjectChecks>();
  readonly history = new Map<string, ProjectChecks[]>();
  readonly drafts = new Map<string, ChecksDraft>();
  readonly jobs = new Map<string, CheckJob>();
  /** Synthetic current input contents, represented by their fingerprints. */
  readonly inputHashes = new Map<string, string | null>();
  refuseNext: string | null = null;
  uncertainStop = false;
  /** Whether the computer can still see an uncertain check's process. */
  uncertainProcessRunning = false;
  private timers: ReturnType<typeof setTimeout>[] = [];
  private requests = new Map<string, { projectId: string; checkId: string | null; request: string; base: ProjectChecks }>();

  constructor(private host: SimHost, checkingTask = false) {
    const fixtures = checksFixtures(host, Date.now(), checkingTask);
    for (const approved of fixtures.approved) {
      this.approved.set(approved.project_id, approved);
      this.history.set(approved.project_id, [structuredClone(approved), ...(approved.version > 1 ? [{ ...structuredClone(approved), version: approved.version - 1, changed_at: approved.changed_at - 86400_000 }] : [])]);
    }
    for (const draft of fixtures.drafts) this.drafts.set(draft.id, draft);
    for (const job of fixtures.jobs) this.jobs.set(job.id, job);
    this.inputHashes.set(`${ids.pTide}/package.json`, "b".repeat(64));
  }

  dispose(): void { for (const timer of this.timers) clearTimeout(timer); }
  private later(ms: number, fn: () => void): void { this.timers.push(setTimeout(fn, ms)); }
  private project(id: string) {
    const project = this.host.projects.find(project => project.project_id === id);
    if (!project) throw error("not_found", "That project is no longer on this computer.");
    return project;
  }
  private current(id: string): ProjectChecks { this.project(id); return this.approved.get(id) ?? emptyChecks(id); }
  private draft(id: string): ChecksDraft {
    const draft = this.drafts.get(id);
    if (!draft) throw error("not_found", "That suggestion is no longer available.");
    this.project(draft.project_id);
    return draft;
  }
  private job(id: string): CheckJob {
    const job = this.jobs.get(id);
    if (!job) throw error("not_found", "That check run is no longer available.");
    this.project(job.project_id);
    return job;
  }
  private check(approved: ProjectChecks, id: string): Check {
    const check = approved.checks.find(check => check.check_id === id);
    if (!check) throw error("not_found", "That check is no longer available.");
    return check;
  }
  private draftView(draft: ChecksDraft): ChecksDraftView {
    const base = this.history.get(draft.project_id)?.find(version => version.version === draft.base_version);
    return { draft, differences: draft.items.map(item => item.kind === "remove" ? ["Removes this check"] : checkDifferences(base?.checks.find(check => check.check_id === item.check.check_id), item.check)),
      facts: draft.items.map(item => item.kind === "remove" ? null : checkFacts(item.check)) };
  }
  private projectView(id: string): ProjectChecksView {
    const approved = this.current(id);
    const writer = this.project(id).active_writer;
    const projectJobs = [...this.jobs.values()].filter(job => job.project_id === id);
    const unfinished = projectJobs.filter(job => active(job) || job.state === "uncertain").sort((a, b) => b.created_at - a.created_at);
    const unfinishedJob = unfinished.find(job => writer != null && (job.id === writer || job.run_id === writer)) ?? unfinished[0];
    const jobs = projectJobs.filter(job => job.purpose.kind !== "draft_proof").sort((a, b) => (b.finished_at ?? b.created_at) - (a.finished_at ?? a.created_at));
    return { project_id: id, project_name: this.project(id).name, approved, ...(unfinishedJob ? { unfinished_job: unfinishedJob } : {}),
      checks: approved.checks.map(check => {
        const job = jobs.find(job => job.results.some(result => result.check_id === check.check_id));
        const result = job?.results.find(result => result.check_id === check.check_id) ?? null;
        const changed = this.changedInputs(approved, check);
        return { check, facts: checkFacts(check), last_result: result, last_task_id: job?.task_id ?? null,
          last_task_title: this.host.tasks.find(task => task.task_id === job?.task_id)?.title ?? null, last_finished_at: job?.finished_at ?? null, inputs_changed: changed };
      }), drafts: [...this.drafts.values()].filter(draft => draft.project_id === id && open(draft)).map(draft => this.draftView(draft)) };
  }
  query(q: ChecksQuery): QueryData {
    let result: QueryData;
    switch (q.kind) {
      case "checks_list": {
        const projects = q.project_id ? [this.projectView(q.project_id)] : this.host.projects.filter(project => !project.home).map(project => this.projectView(project.project_id));
        result = { kind: q.kind, projects: q.project_id ? projects : projects.filter(project => project.checks.length || project.drafts.length), projects_without_checks: projects.filter(project => !project.checks.length).length };
        break;
      }
      case "checks_history": this.project(q.project_id); result = { kind: q.kind, versions: this.history.get(q.project_id) ?? [] }; break;
      case "task_checks":
        if (!this.host.tasks.some(task => task.task_id === q.task_id)) throw error("not_found", "That task is no longer available.");
        result = { kind: q.kind, jobs: [...this.jobs.values()].filter(job => job.task_id === q.task_id), drafts: [...this.drafts.values()].filter(draft => draft.task_id === q.task_id && open(draft)).map(draft => this.draftView(draft)) }; break;
      case "check_job": result = { kind: q.kind, job: this.job(q.job_id) }; break;
    }
    // A wire response is a snapshot, not a live reference to the simulator.
    return structuredClone(result);
  }

  async act(c: ChecksCommand, grants: Grants): Promise<CommandResult> {
    if (!grants.library_write || (c.command === "checks_ask" && !grants.tasks)) throw error("permission_denied", "This device does not have permission to change checks.");
    if (this.refuseNext) { const message = this.refuseNext; this.refuseNext = null; throw error("permission_denied", message); }
    switch (c.command) {
      case "checks_ask": return this.ask(c);
      case "checks_discard_draft": {
        const draft = this.draft(c.draft_id);
        if (!open(draft)) throw error("conflict", "This suggestion has already been used or discarded.");
        draft.state = "discarded"; draft.updated_at = Date.now();
        for (const job of this.jobs.values()) if (job.purpose.kind === "draft_proof" && job.purpose.draft_id === draft.id && active(job)) this.stop(job);
        return { kind: "checks_draft", draft: structuredClone(draft) };
      }
      case "checks_stop_job": {
        const job = this.job(c.job_id);
        if (job.purpose.kind === "after_run") throw error("invalid_request", "Stop the task to stop its checks.");
        if (c.confirmed_stopped) {
          if (job.state !== "uncertain") throw error("conflict", "Only confirm a check whose stop could not be confirmed.");
          if (this.uncertainProcessRunning) throw error("conflict", "Ophio can still see the check running. The project is still reserved.");
          job.state = "finished"; job.message = "Not checked: stopped"; job.finished_at = Date.now(); job.process = null;
          job.results = job.results.map(result => result.outcome.kind === "could_not_run" ? { ...result, outcome: { kind: "stopped" }, output: "You confirmed that this check stopped. It was not checked." } : result);
          this.release(job);
        } else if (job.state !== "uncertain") this.stop(job);
        return { kind: "check_job", job: structuredClone(job) };
      }
      case "checks_run_now": {
        const approved = this.current(c.project_id);
        this.assertFree(c.project_id);
        if (new Set(c.check_ids).size !== c.check_ids.length) throw error("invalid_request", "Choose each check only once.");
        const checks = c.check_ids.length ? c.check_ids.map(id => this.check(approved, id)) : approved.checks.filter(check => check.enabled && check.when === "every_task");
        if (!checks.length) throw error("invalid_request", "There are no checks to run yet.");
        return { kind: "check_job", job: this.startJob(approved, checks) };
      }
      case "checks_apply_draft": {
        const draft = this.draft(c.draft_id), previous = this.current(draft.project_id);
        if (draft.state === "stale" || draft.base_version !== previous.version || c.base_version !== previous.version) throw stale();
        if (draft.state !== "ready") throw error("conflict", "Wait until Ophio has checked this suggestion before using it.");
        if (!c.keep_items.length || new Set(c.keep_items).size !== c.keep_items.length || c.keep_items.some(index => !Number.isInteger(index) || index < 0 || index >= draft.items.length)) throw error("invalid_request", "Choose at least one suggestion, and choose each only once.");
        const next = structuredClone(previous), refreshed: string[] = [];
        for (const [index, item] of draft.items.entries()) {
          if (!c.keep_items.includes(index)) continue;
          if (item.kind === "remove") { this.check(next, item.check_id); next.checks = next.checks.filter(check => check.check_id !== item.check_id); }
          else {
            this.validate(item.check);
            const exists = next.checks.some(check => check.check_id === item.check.check_id);
            if ((item.kind === "add") === exists) throw error("invalid_request", "This suggestion no longer matches the approved checks.");
            if (exists) next.checks = next.checks.map(check => check.check_id === item.check.check_id ? structuredClone(item.check) : check);
            else next.checks.push(structuredClone(item.check));
            refreshed.push(item.check.check_id);
          }
        }
        this.fingerprints(next, refreshed);
        draft.state = "applied"; draft.updated_at = Date.now();
        const approved = this.commit(next, { kind: "draft", draft_id: draft.id, device_id: ids.device });
        return { kind: "checks_saved", approved, job_id: null };
      }
      case "checks_save": {
        const previous = this.current(c.project_id);
        if (c.base_version !== previous.version) throw stale();
        let next = structuredClone(previous);
        const edit = c.edit;
        switch (edit.kind) {
          case "undo": {
            const prior = previous.version === 1 ? emptyChecks(c.project_id) : this.history.get(c.project_id)?.[1];
            if (!prior) throw error("conflict", "There is no earlier change to undo.");
            next = structuredClone(prior); break;
          }
          case "save": {
            this.validate(edit.check);
            // Passing durations are observations, not editable claims.
            const check = { ...structuredClone(edit.check), typical_secs: previous.checks.find(check => check.check_id === edit.check.check_id)?.typical_secs ?? null };
            next.checks = next.checks.some(item => item.check_id === check.check_id) ? next.checks.map(item => item.check_id === check.check_id ? check : item) : [...next.checks, check];
            this.fingerprints(next, [check.check_id]); break;
          }
          case "delete": this.check(next, edit.check_id); next.checks = next.checks.filter(check => check.check_id !== edit.check_id); this.fingerprints(next, []); break;
          case "set_enabled": this.check(next, edit.check_id).enabled = edit.enabled; break;
          case "keep_inputs": this.check(next, edit.check_id); this.fingerprints(next, [edit.check_id]); break;
        }
        const approved = this.commit(next, { kind: "person", device_id: ids.device });
        const job = edit.kind === "save" ? this.startJob(approved, [this.check(approved, edit.check.check_id)], true) : null;
        return { kind: "checks_saved", approved, job_id: job?.id ?? null };
      }
    }
  }

  private validate(check: Check): void {
    if (!/^[\da-f]{8}-[\da-f]{4}-[\da-f]{4}-[\da-f]{4}-[\da-f]{12}$/i.test(check.check_id) || !check.name.trim() || !check.about.trim() || !check.run.trim()) throw error("invalid_request", "Give the check a name, a description and something to run.");
    if (!Number.isInteger(check.timeout_secs) || check.timeout_secs < 10 || check.timeout_secs > 3600) throw error("invalid_request", "The time limit must be between 10 seconds and 1 hour.");
    if ([check.name, check.about, check.run, check.setup ?? "", ...check.paths, ...check.inputs].some(value => /\u0000/.test(value))) throw error("invalid_request", "The check contains text that cannot be saved.");
    if (check.inputs.some(path => !path || /^(?:[\\/]|[a-z]:)/i.test(path) || path.split(/[\\/]/).includes(".."))) throw error("invalid_request", "Files a check depends on must stay inside the project.");
    if (check.paths.some(path => !path || path.includes("[") !== path.includes("]") || path.includes("{") !== path.includes("}"))) throw error("invalid_request", "One of the watched file patterns is incomplete.");
  }
  private changedInputs(approved: ProjectChecks, check: Check): string[] {
    return check.inputs.filter(path => (approved.input_fingerprints.find(input => input.check_id === check.check_id && input.path === path)?.sha256 ?? null) !== (this.inputHashes.get(`${approved.project_id}/${path}`) ?? null));
  }
  private fingerprints(next: ProjectChecks, refresh: string[]): void {
    next.input_fingerprints = next.input_fingerprints.filter(input => next.checks.some(check => check.check_id === input.check_id && check.inputs.includes(input.path)) && !refresh.includes(input.check_id));
    for (const check of next.checks) if (refresh.includes(check.check_id)) for (const path of check.inputs) next.input_fingerprints.push({ check_id: check.check_id, path, sha256: this.inputHashes.get(`${next.project_id}/${path}`) ?? null });
  }
  private commit(next: ProjectChecks, changedBy: ProjectChecks["changed_by"]): ProjectChecks {
    const current = this.current(next.project_id);
    const approved = structuredClone({ ...next, version: current.version + 1, changed_at: Date.now(), changed_by: changedBy });
    this.approved.set(approved.project_id, approved);
    this.history.set(approved.project_id, [structuredClone(approved), ...(this.history.get(approved.project_id) ?? [])].slice(0, 20));
    for (const draft of this.drafts.values()) if (draft.project_id === approved.project_id && open(draft) && draft.base_version !== approved.version) { draft.state = "stale"; draft.updated_at = Date.now(); }
    return structuredClone(approved);
  }
  private assertFree(projectId: string): void {
    if (this.project(projectId).active_writer || [...this.jobs.values()].some(job => job.project_id === projectId && active(job))) throw error("conflict", "A task or another check is working in this project. Try again when it has finished.");
  }
  private publishWriter(projectId: string): void {
    const project = this.project(projectId);
    this.host.emit({ type: "project_upserted", project: structuredClone(project) });
    const queue = this.host.talk.queue(projectId);
    queue.writer = project.active_writer;
    this.host.talk.put(queue);
  }
  private startJob(approved: ProjectChecks, checks: Check[], wait = false): CheckJob {
    const job: CheckJob = { id: uuidv7(), project_id: approved.project_id, task_id: null, run_id: null,
      purpose: { kind: "run_now", check_ids: checks.map(check => check.check_id) }, checks_version: approved.version, checks: structuredClone(checks), state: "pending", process: null,
      created_at: Date.now(), started_at: null, finished_at: null, sandbox: null, results: [], message: null };
    this.jobs.set(job.id, job);
    const begin = () => {
      if (job.state !== "pending") return;
      const project = this.project(job.project_id);
      if (project.active_writer) { if (wait) this.later(500, begin); return; }
      project.active_writer = job.id;
      this.publishWriter(project.project_id);
      job.state = "running"; job.started_at = Date.now();
      job.sandbox = this.host.variant === "windows" ? { kind: "unsandboxed", reason: "This computer cannot isolate checks yet." } : { kind: "contained" };
      job.process = { pid: 4173, start_time: "sim-process-4173", sandbox_unit: this.host.variant === "windows" ? null : "sim-checks-run-now" };
      const step = () => {
        if (job.state !== "running") return;
        const check = job.checks[job.results.length];
        if (check) job.results.push({ ...sampleResult(check), inputs_changed: this.changedInputs(approved, check) });
        if (job.results.length < job.checks.length) this.later(700, step);
        else { job.state = "finished"; job.finished_at = Date.now(); job.process = null; this.release(job); }
      };
      this.later(700, step);
    };
    // Reserve now, before another command could admit a writer.
    if (!this.project(job.project_id).active_writer) { this.project(job.project_id).active_writer = job.id; this.publishWriter(job.project_id); }
    this.later(300, () => {
      if (job.state !== "pending") return;
      if (this.project(job.project_id).active_writer === job.id) this.project(job.project_id).active_writer = null;
      begin();
    });
    return structuredClone(job);
  }
  private release(job: CheckJob): void {
    const project = this.project(job.project_id);
    if (project.active_writer === job.id || project.active_writer === job.run_id) { project.active_writer = null; this.publishWriter(project.project_id); }
  }
  private stop(job: CheckJob, settled?: () => void): void {
    if (!active(job) || job.state === "cancelling") return;
    // Pending jobs have no process to reconcile. Running jobs retain the writer
    // until the simulated termination check returns, just like the computer.
    const uncertain = job.state !== "pending" && this.uncertainStop;
    const finish = () => {
      job.state = uncertain ? "uncertain" : "finished";
      job.message = uncertain ? "Ophio could not confirm that the checks stopped. They may still be running." : "Not checked: stopped";
      job.finished_at = Date.now();
      for (const check of job.checks) if (!job.results.some(result => result.check_id === check.check_id)) job.results.push({ check_id: check.check_id, name: check.name,
        outcome: uncertain ? { kind: "could_not_run", reason: "Ophio could not confirm that this check stopped." } : { kind: "stopped" },
        duration_secs: 0, exit_code: null, output: uncertain ? "The check may still be running; its result is not confirmed." : "Stopped before this check finished.", inputs_changed: [] });
      if (!uncertain) { job.process = null; this.release(job); }
      settled?.();
    };
    if (job.state === "pending") finish();
    else {
      job.state = "cancelling";
      job.message = "Stopping checks…";
      this.later(500, finish);
    }
  }

  /** Checking is a detail of Working, so the ordinary Stop remains valid. */
  stopTask(taskId: string): boolean {
    const task = this.host.tasks.find(task => task.task_id === taskId);
    if (!task?.checking) return false;
    const job = this.job(task.checking.job_id);
    const queue = this.host.talk.queue(task.project_id);
    queue.paused = true;
    queue.pause_reason = `Next up paused when you stopped “${task.title}”, so nothing starts by itself. Resume when you’re ready.`;
    this.host.talk.invalidate(queue); this.host.talk.put(queue);
    this.stop(job, () => {
      const run = this.host.runs.find(run => run.run_id === task.current_run);
      if (run) { Object.assign(run, { state: job.state === "uncertain" ? "reconciliation_required" : "completed", ended_at: Date.now(), holds_writer: job.state === "uncertain" }); this.host.emit({ type: "run_upserted", run: structuredClone(run) }); }
      const item: AttentionItem = { attention_id: uuidv7(), kind: job.state === "uncertain" ? "reconciliation_required" : "completed", title: task.title, body: job.message,
        task_id: taskId, run_id: task.current_run, approval_id: null, created_at: Date.now(), resolved_at: null, priority: job.state === "uncertain" ? 1 : 5 };
      this.host.attention.push(item);
      this.host.emit({ type: "attention_upserted", item });
      this.host.touchTask(taskId, { checking: undefined, state: job.state === "uncertain" ? "reconciliation_required" : "completed", status_line: job.message });
    });
    return true;
  }

  private async ask(c: Extract<ChecksCommand, { command: "checks_ask" }>): Promise<CommandResult> {
    const project = this.project(c.project_id), approved = this.current(c.project_id);
    const selected = c.check_id ? this.check(approved, c.check_id) : null;
    const setup = !c.request.trim() && !approved.checks.length;
    if (!setup && !c.request.trim()) throw error("invalid_request", "Say what you would like changed.");
    if (c.request.length > 32_768) throw error("invalid_request", "That request is too long. Try a shorter description.");
    const prompt = setup ? "Set up checks for this project. Suggest checks for me to approve; do not change the approved checks."
      : `${selected ? `Change the check “${selected.name}”.\n\n` : "Change this project’s checks.\n\n"}${c.request}`;
    const made = await this.host.inside({ command: "create_task", project_id: project.project_id, kind: "coding", mode: "work", title: setup ? "Set up checks" : "Change checks", prompt, bundle: null,
      harness: project.default_harness ?? "claude_code", model: null, effort: null, target_id: null, mission: null,
      placement: project.active_writer ? { mode: "queue", position: null, authorization: "auto_start" } : { mode: "start_now" } });
    if (made?.kind !== "task_created") throw error("internal", "The checks task could not be created.");
    this.requests.set(made.task_id, { projectId: c.project_id, checkId: c.check_id, request: c.request, base: structuredClone(approved) });
    return made;
  }
  reply(taskId: string) {
    if (!this.requests.has(taskId)) return null;
    return { activity: [{ tool: "checks_propose", summary: "Suggested checks for you to review" }], text: "I have suggested checks for you to review. Nothing in your approved checks has changed.", summary: "Suggested checks are ready to review" };
  }
  finishedTask(taskId: string): void {
    const request = this.requests.get(taskId);
    if (!request) return;
    this.requests.delete(taskId);
    const selected = request.checkId ? request.base.checks.find(check => check.check_id === request.checkId) : null;
    const candidate = selected ? { ...structuredClone(selected), timeout_secs: selected.timeout_secs === 600 ? 900 : 600 }
      : { ...sampleChecks()[2]!, check_id: uuidv7(), typical_secs: null };
    const draft: ChecksDraft = { id: uuidv7(), project_id: request.projectId, base_version: request.base.version, task_id: taskId, request: request.request,
      items: selected ? [{ kind: "change", check: candidate, note: "Gives this check a different time limit." }] : [{ kind: "add", check: candidate, note: "Checks that the app can still be built." }],
      state: this.current(request.projectId).version === request.base.version ? "ready" : "stale", proofs: [sampleResult(candidate)], created_at: Date.now(), updated_at: Date.now() };
    for (const old of this.drafts.values()) if (old.task_id === taskId && open(old)) old.state = "discarded";
    this.drafts.set(draft.id, draft);
  }
}
