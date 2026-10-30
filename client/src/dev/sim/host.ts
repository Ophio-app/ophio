// SPDX-License-Identifier: Apache-2.0
//
// Development-only simulated companion. It keeps a journal, answers queries
// and admits commands with the same shapes the real companion uses, and
// scripts plausible agent behavior so every screen can be exercised without
// a computer. It never makes a network or model call.

import { ProblemReportsSim, SIM_APP_BYTES, SIM_APP_SHA } from "./problemReports";
import { PRODUCT_NAME } from "../../brand";
import { blockingAttentionCount } from "./attention";
import { stopFrequency } from "../../screens/control/stops";
import { isTerminalRunState, stillFinishing } from "../../state/projection";

import type {
  AgentPreferences,
  NotificationSettings,
  AnnotationBundle,
  Approval,
  AttentionItem,
  Availability,
  Command,
  CommandEnvelope,
  CommandReceipt,
  CommandStage,
  CommandResult,
  ControlTarget,
  DirEntry,
  Event,
  EventEnvelope,
  EventSource,
  FileContent,
  HarnessInfo,
  InputAck,
  InputEnvelope,
  InterventionReceipt,
  LibraryRoot,
  LiveEvent,
  PreviewEndpoint,
  Project,
  Provenance,
  ProtocolError,
  Query,
  QueryData,
  QueueView,
  ReadinessCheck,
  ReadinessReport,
  ReceiptFact,
  Run,
  RunState,
  ServerFrame,
  TargetControlView,
  Task,
  TaskRecords,
  TaskMode,
  TaskState,
  TerminalInfo,
  TimelineBody,
  TimelineItem,
  ArtifactMeta,
  DownloadSource,
  Grants,
  ConversationAccess,
  TransferRecord,
} from "../../protocol/types";
import { uuidv7 } from "../../protocol/ids";
import * as fx from "./scenario";
import { SimDisabled } from "./disabled";
import { SimLibraryWorkflows, libraryHash } from "./libraryWorkflows";
import { SimLibrary } from "./library";
import { SimLibraryCompletion } from "./libraryCompletion";
import { UploadStaging } from "./transfers";
import { TalkSim } from "./talk";
import { WorkSim } from "./work";
import { ChildrenSim } from "./children";
import { ComputerSim, validateComputerStart } from "./computer";
import { ControlDrills } from "./control";
import { captureTarget } from "./annotate";
import { simCatalog } from "./harnesses";
import { AgentUpdatesSim } from "./agentUpdates";
import { VoiceSim } from "./voice";
import { settingsCommand, settingsFixtures, settingsQuery } from "./settings";
import { variantHost, variantChecks, type HostVariant } from "./variants";

import { TabletSim } from "./tablet";
import { StopSafetySim } from "./stopSafety";
import { WindowsSim } from "./windows";
import { StageSim } from "./stages";
import { ContributionSim } from "./contribution";
import { AccountsSim } from "./accounts";
import { ChecklistSim } from "./checklist";
import { AgentLoginsSim } from "./agentLogins";
import { GithubSim, githubTaskBranch } from "./github";
import { SystemSim } from "./system";
import { projectFolders } from "./projectFolders";
import { catalog, chooseSetupSkillVersion, copySetupSkill, createAgent, oneOff, resolveStart, sameStart, setupCopyPath } from "./launchers";

type Frame = ServerFrame;
type Listener = (f: Frame) => void;

const HARNESS_SOURCE: EventSource = { kind: "harness", harness: "claude_code" };
const COMPANION: EventSource = { kind: "companion" };
const DEVICE: EventSource = { kind: "device", device_id: fx.ids.device };

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Commands the companion settles after admission (`pending` in
 * crates/services/src/commands.rs). Their receipt carries no result; the
 * outcome arrives later as `command_progress`, so the sim does the same. */
const SETTLED_LATER: ReadonlySet<Command["command"]> = new Set<Command["command"]>([
  "request_human_lease",
  "release_lease",
  "pause_input",
  "take_over",
  "return_to_agent",
  "start_private_step",
  "end_private_step",
  "withdraw_observation",
  "lock_desktop",
  "unlock_desktop",
  "capture_target",
  "library_save",
  "library_create",
  "library_rename",
  "library_delete",
  "make_reusable",
  // As the computer: it says what it made once it has, not on accepting.
  "create_agent",
  "copy_setup_skill",
  "choose_setup_skill_version",
  "open_terminal",
  "close_terminal",
  "run_readiness_check",
  "window_action",
  "account",
  "agent_login",
]);
const SETTLE_MS = 150;
/** As the computer, which may stop a working agent's turn and continue it to
 * put an access change in force. */
const ACCESS_WORKING_MS = 1500;
/** As the companion refuses access on anything but a conversation, or a task carried on from one. */
const ONLY_CONVERSATIONS = "Only a conversation is held to chosen folders; a task follows its mode";

/** As ConversationAccess::normalized: what it may change it may read. */
function normalizedAccess(a: ConversationAccess): ConversationAccess {
  const read = [...new Set([...a.read, ...a.write])].sort();
  const write = [...new Set(a.write)].sort();
  return { ...a, read, write, whole_read: a.whole_read || a.whole_write };
}

/** As a conversation turned into a task keeps its access, with its own project to change. */
function carriedAccess(a: ConversationAccess, projectId: string): ConversationAccess {
  return normalizedAccess({ ...a, write: [...a.write, projectId] });
}

/** As the companion refuses files under Home (commands.rs not_home). */
const HOME_FILES = "Files in your home folder aren’t sent to or from the phone. Choose a project’s folder, or Library’s.";
/** The simulated computer's password, for unlocking it from Control. */
export const SIM_DESKTOP_PASSWORD = "synthetic desktop password";
const UNLOCK_TRIES = 3;
const UNLOCK_WINDOW_MS = 15 * 60_000;

export async function sha256(text: string | Uint8Array): Promise<string> {
  if (globalThis.crypto?.subtle) {
    const bytes = typeof text === "string" ? new TextEncoder().encode(text) : text;
    const d = await crypto.subtle.digest("SHA-256", bytes as Uint8Array<ArrayBuffer>);
    return [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, "0")).join("");
  }
  // Fallback for non-secure contexts: FNV-1a, clearly not a real revision.
  let h = 0x811c9dc5;
  const units = typeof text === "string" ? [...text].map((c) => c.charCodeAt(0)) : [...text];
  for (const u of units) h = Math.imul(h ^ u, 0x01000193);
  return (h >>> 0).toString(16).padStart(64, "0");
}

function err(code: ProtocolError["code"], message: string): ProtocolError {
  return { code, message };
}

export interface SimOptions {
  /** Multiplier for scripted delays; 0 makes scripts run immediately. */
  pace?: number;
  variant?: HostVariant;
  indicator?: boolean;
}

export class SimHost {
  readonly stages: StageSim;
  readonly contribution: ContributionSim;
  readonly accounts: AccountsSim;
  readonly checklists: ChecklistSim;
  readonly agentLogins: AgentLoginsSim;
  readonly github: GithubSim;
  readonly stopSafety = new StopSafetySim(this);
  readonly tablet = new TabletSim(this);
  readonly variant: HostVariant;
  readonly indicator: boolean;
  readonly uploadTasks = new Map<string, string>();
  readonly windows = new WindowsSim(this);
  readonly system = new SystemSim(Date.now());
  harnesses: HarnessInfo[] = simCatalog(fx.harnesses);
  /** Whether Jev, which voice control asks, is linked on the computer. */
  jev: { connection: "connected" | "not_connected" | "last_check_failed"; checked_at: number | null } = { connection: "not_connected", checked_at: null };
  readonly reports = new ProblemReportsSim(this);
  readonly agentUpdates = new AgentUpdatesSim();
  agentPreferences: AgentPreferences = { questions: true, revision: 0 };
  notificationSettings: NotificationSettings = { off: [], bundle: false, projects: [], revision: 0 };
  readonly voice = new VoiceSim();
  readonly work: WorkSim;
  readonly children: ChildrenSim;
  readonly talk: TalkSim;
  readonly controlSim: ControlDrills;
  readonly computer: ComputerSim;
  seq = 0;
  readonly journal: EventEnvelope[] = [];
  readonly bootEpoch = fx.ids.boot;

  host;
  projects: Project[];
  tasks: Task[];
  runs: Run[];
  approvals: Approval[];
  /** One failed bridge hand-off for approval delivery drills. */
  failNextApprovalDelivery = false;
  attention: AttentionItem[];
  targets: ControlTarget[];
  control: TargetControlView[];
  roots: LibraryRoot[];
  files: Record<string, Record<string, string>>;
  skillBases: Record<string, string> = {};
  readonly library: SimLibrary;
  readonly disabled = new SimDisabled(this);
  readonly #startPaths = new Map<string, string>();
  readonly libraryCompletion = new SimLibraryCompletion(this);
  readonly libraryWorkflows: SimLibraryWorkflows;
  readonly uploadStaging = new UploadStaging();
  terminals: TerminalInfo[];
  queue: QueueView;
  previews: PreviewEndpoint[];
  transfers: TransferRecord[] = [];
  /** The next upload's final move ends unconfirmed (a crash mid-publication). */
  interruptPublication = false;
  /** Bytes of artifacts created during this simulation, so they can be downloaded. */
  artifactBytes = new Map<string, Uint8Array>();
  readonly uploadEvidence = new Map<string, import("../../protocol/types").ArtifactLink>();
  #downloads = new Map<string, { bytes: Uint8Array; sha256: string }>();
  readiness: ReadinessReport;
  devices;
  identities;
  links;

  readonly receipts = new Map<string, CommandReceipt>();
  readonly #settled = new Map<string, { stage: CommandStage; result?: CommandResult; error?: ProtocolError }>();
  readonly #listeners = new Set<Listener>();
  #pace: number;
  #timers: ReturnType<typeof setTimeout>[] = [];
  #pendingMessages = new Map<string, Map<number, "queued_for_boundary" | "host_accepted">>();
  #startMessages = new Map<string, number>();
  #unlockTries: number[] = [];
  /** How long checking a password takes; a drill slows it to act while one
   * is being checked. */
  unlockCheckMs = SETTLE_MS;
  #queuedStartMessages = new Map<string, number>();

  trackStartMessage(taskId: string, seq: number): void { this.#queuedStartMessages.set(taskId, seq); }
  #leaseGen = 10;

  constructor(opts: SimOptions = {}) {
    this.#pace = opts.pace ?? 1;
    this.variant = opts.variant ?? "hyprland";
    this.indicator = opts.indicator ?? true;
    const now = Date.now();
    this.host = variantHost(fx.hostInfo(now), this.variant, this.indicator);
    this.host.capabilities.window_actions = this.windows.capabilities();
    this.projects = fx.projects(now);
    this.stages = new StageSim(this);
    this.contribution = new ContributionSim(this);
    this.accounts = new AccountsSim(this);
    this.checklists = new ChecklistSim(this);
    this.agentLogins = new AgentLoginsSim(this);
    this.github = new GithubSim(this, Date.now());
    this.tasks = fx.tasks(now);
    this.runs = fx.runs(now).map(run => ({ ...run, force_stoppable: true }));
    this.approvals = fx.approvals(now);
    this.attention = fx.attention(now);
    this.targets = fx.targets();
    this.control = fx.control(now);
    if (!this.indicator) this.control = this.control.map((v) => ({ ...v, input_state: "paused", lease: null, model_observation: v.model_observation ? { ...v.model_observation, state: "withdrawn" } : null }));
    this.roots = fx.libraryRoots.map((r) => ({ ...r }));
    this.files = structuredClone(fx.libraryFiles);
    this.library = new SimLibrary(this);
    this.libraryWorkflows = new SimLibraryWorkflows(this);
    this.terminals = fx.terminals(now);
    this.queue = fx.queue(now);
    const settings = settingsFixtures(allGrants, now);
    this.devices = settings.devices;
    this.identities = settings.identities;
    this.links = settings.links;
    this.previews = [...fx.previews(now), ...settings.previews];
    this.readiness = this.#readiness(now - 3 * 60_000);
    // Seed timelines as journaled events so item seqs are real journal seqs.
    const seeds = fx.timelines();
    const all: { taskId: string; at: number; runId: string | null; body: TimelineBody }[] = [];
    for (const [taskId, items] of Object.entries(seeds)) {
      for (const s of items) all.push({ taskId, at: now - s.ago, runId: s.runId, body: s.body });
    }
    all.sort((a, b) => a.at - b.at);
    for (const s of all) this.#append(s.taskId, s.runId, s.body, s.at, true);
    this.work = new WorkSim(this);
    this.children = new ChildrenSim(this);
    this.talk = new TalkSim(this, (task, prompt) => this.#startRun(task, prompt));
    this.controlSim = new ControlDrills(this);
    this.computer = new ComputerSim(this);
    try {
      const saved: Record<string, NonNullable<Run["human_resolution"]>> = JSON.parse(localStorage.getItem("sim.human-decisions") ?? "{}");
      for (const run of this.runs) {
        const decision = saved[run.run_id];
        if (!decision || !["mark_completed", "mark_failed"].includes(decision.resolution) || !Number.isFinite(decision.at)) continue;
        const state = decision.resolution === "mark_failed" ? "failed" : "completed";
        Object.assign(run, { human_resolution: decision, state, ended_at: decision.at, holds_writer: false });
        const task = this.tasks.find(t => t.current_run === run.run_id);
        if (task) Object.assign(task, { state });
        const project = this.projects.find(p => p.active_writer === run.run_id);
        if (project) project.active_writer = null;
        for (const item of this.attention) if (item.run_id === run.run_id && item.kind === "reconciliation_required") item.resolved_at = decision.at;
      }
    } catch { /* Ignore unreadable fixture storage. */ }
    for (const task of this.tasks) task.attention_count = blockingAttentionCount(this.attention, task.task_id);
    this.#startBackground();
  }

  setPace(multiplier: number): void {
    if (!Number.isFinite(multiplier) || multiplier <= 0) throw new Error("Pace must be positive");
    this.#pace = multiplier;
  }

  dispose(): void {
    this.reports.dispose();
    this.stopSafety.dispose();
    this.controlSim.dispose();
    this.accounts.dispose();
    this.contribution.dispose();
    this.computer.dispose();
    this.github.dispose();
    this.checklists.dispose();
    for (const t of this.#timers) clearTimeout(t);
    this.#timers = [];
    this.#listeners.clear();
  }

  // ---- Connection --------------------------------------------------------

  attach(listener: Listener): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  #broadcast(f: Frame): void {
    for (const l of this.#listeners) l(f);
  }

  replayAfter(afterSeq: number | null, send: Listener): void {
    const from = afterSeq ?? this.seq;
    const oldest = this.journal[0]?.seq ?? this.seq + 1;
    if (afterSeq !== null && afterSeq + 1 < oldest) {
      send({ type: "journal_gap", requested_from: afterSeq + 1, oldest_retained: oldest });
      return;
    }
    for (const e of this.journal) if (e.seq > from) send({ type: "event", ...e });
    send({ type: "caught_up", head: this.seq });
  }

  // ---- Journal -----------------------------------------------------------

  emit(event: Event, source: EventSource = COMPANION, provenance: Provenance = "native", at = Date.now()): number {
    const env: EventEnvelope = structuredClone({ seq: ++this.seq, at, source, provenance, event });
    this.journal.push(env);
    if (this.journal.length > 4000) this.journal.splice(0, 1000);
    this.#broadcast({ type: "event", ...env });
    return env.seq;
  }

  live(ev: LiveEvent): void {
    this.#broadcast({ type: "live", ...ev });
  }

  #append(taskId: string, runId: string | null, body: TimelineBody, at = Date.now(), seeding = false): TimelineItem {
    const item: TimelineItem = { seq: this.seq + 1, at, run_id: runId, item: body };
    this.emit({ type: "timeline_appended", task_id: taskId, item }, body.kind === "user_message" ? DEVICE : HARNESS_SOURCE, "native", at);
    if (!seeding) this.#touchTask(taskId, {});
    return item;
  }

  #later(ms: number, fn: () => void): void {
    this.#timers.push(setTimeout(fn, ms * this.#pace));
  }

  // ---- Queries -----------------------------------------------------------

  async query(q: Query): Promise<QueryData> {
    switch (q.kind) {
      // A simulated receiver for drills and screenshots: it downloads nothing.
      case "contribution_delivery": throw err("unsupported", "The simulator does not fetch contribution links");
      case "contribution_capabilities": return { kind: "contribution_capabilities", capabilities: this.contribution.capabilities() };
      case "contribution_operation": return { kind: "contribution_operation", operation: this.contribution.view(q.operation_id) };
      case "contribution_policy": return { kind: "contribution_policy", document: this.contribution.policy(q.operation_id, q.path) };
      case "project_folders": return { kind: "project_folders", listing: projectFolders(q, this.projects) };
      case "project_stages": return this.stages.query(q);
      case "accounts": return this.accounts.query();
      case "checklists":
      case "checklist":
      case "file_delivery":
        return this.checklists.query(q);
      case "agent_logins": return this.agentLogins.query();
      case "agent_login_password": return this.agentLogins.password(q.login_id);
      case "github_project": return { kind: "github_project", view: this.github.project(q.project_id) };
      case "github_items": return { kind: "github_items", items: this.github.list(q.project_id, q.list, q.filter) };
      case "github_pr": return { kind: "github_pr", pr: this.github.pr(q.project_id, q.number) };
      case "voice": return { kind: "voice", view: this.voice.view(this, q.session_id) };
      case "windows": return { kind: "windows", catalog: this.windows.catalog(q.search) };
      case "observed_sessions": return { kind: "observed_sessions", view: this.children.query() };
      case "agent_updates": return { kind: "agent_updates", view: structuredClone(this.agentUpdates.view) };
      case "agent_preferences": return { kind: "agent_preferences", view: { ...this.agentPreferences } };
      case "notification_settings": return { kind: "notification_settings", view: structuredClone(this.notificationSettings) };
      // Synthetic test submission used by the owner's inbox screen drill.
      // Unknown reports have no code; the simulator must not invent a patch.
      case "inbox_report_code": return { kind: "inbox_report_code", code: q.report_id === "0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b" ? {
        kind: "vitest", path: "client/src/screens/work/report.test.tsx",
        source: "it('keeps the report visible', () => {\n  expect(screen.getByText('Report')).toBeTruthy();\n});",
        patch: "--- a/client/src/screens/work/Attention.tsx\n+++ b/client/src/screens/work/Attention.tsx\n@@ -1 +1 @@\n-old\n+new",
      } : null };
      case "reports":
      case "report":
      case "report_preview":
        return this.reports.query(q);
      case "disabled_in_ophio": return { kind: "disabled_in_ophio", items: this.disabled.list() };
      case "catalog":
        if (q.project_id && !this.projects.some((p) => p.project_id === q.project_id)) throw err("not_found", "That project is no longer on this computer");
        return { kind: "catalog", catalog: await catalog(this, sha256, q.project_id, this.disabled) };
      case "snapshot":
        return {
          kind: "snapshot",
          journal_head: this.seq,
          host: this.host,
          projects: this.projects,
          tasks: this.tasks,
          runs: this.runs,
          approvals: this.approvals,
          attention: this.attention,
          targets: this.targets,
          control: this.control,
          library_roots: this.roots,
        };
      case "command_status":
        return { kind: "command_status", receipt: this.receipts.get(q.command_id) ?? null, ...this.#settled.get(q.command_id) };
      case "task_timeline": {
        const items = this.journal
          .filter((e) => e.event.type === "timeline_appended" && e.event.task_id === q.task_id)
          .map((e) => (e.event as Extract<Event, { type: "timeline_appended" }>).item)
          .filter((i) => q.before_seq === null || i.seq < q.before_seq);
        const page = items.slice(-q.limit);
        return { kind: "task_timeline", items: page, has_more: items.length > page.length };
      }
      case "harnesses":
        return { kind: "harnesses", harnesses: this.harnesses };
      case "control_targets":
        return { kind: "control_targets", targets: this.targets };
      case "library_list":
        return { kind: "library_list", listing: { root_id: q.root_id, dir: q.dir, entries: this.#list(q.root_id, q.dir) } };
      case "library_read": {
        const file = await this.#read(q.root_id, q.path);
        if (!file) throw err("not_found", "That file is no longer there");
        return { kind: "library_read", file };
      }
      case "library_search": {
        const needle = q.text.toLowerCase();
        const hits = [];
        for (const [root, files] of Object.entries(this.files)) {
          if (q.root_id && q.root_id !== root) continue;
          try { this.library.assertRead(root); } catch { continue; }
          for (const [path, text] of Object.entries(files)) {
            // As the companion does: the file's name first, then its lines.
            if (path.split("/").pop()!.toLowerCase().includes(needle))
              hits.push({ root_id: root, path, snippet: "" });
            const lines = text.split("\n");
            for (let i = 0; i < lines.length; i++) {
              if (lines[i]!.toLowerCase().includes(needle))
                hits.push({ root_id: root, path, line: i + 1, snippet: lines[i]!.slice(0, 300) });
            }
          }
        }
        return { kind: "library_search", hits: hits.slice(0, q.limit) };
      }
      case "library_recovery_list":
      case "library_recovery_read":
      case "reusable_formats":
        return this.libraryCompletion.query(q);
      case "instructions":
      case "library_trash":
      case "library_trash_read":
      case "agent_files":
      case "reusable_draft":
      case "upload_destination":
        return this.libraryWorkflows.query(q);
      case "terminals":
        return { kind: "terminals", terminals: this.terminals };
      case "previews":
        return { kind: "previews", previews: this.previews.filter((p) => p.project_id === q.project_id) };
      case "project_branches": {
        const project = this.projects.find((p) => p.project_id === q.project_id);
        if (!project) throw err("not_found", "That project is no longer on this computer");
        return { kind: "project_branches", listing: fx.branchListing(project) };
      }
      case "findings": {
        if (!this.projects.some((p) => p.project_id === q.project_id)) throw err("not_found", "That project is no longer on this computer");
        return { kind: "findings", text: fx.findings[q.project_id] ?? null };
      }
      case "readiness":
        return { kind: "readiness", report: this.readiness };
      case "system_stats":
        return { kind: "system_stats", stats: this.system.stats(Date.now()) };
      case "devices":
        return { kind: "devices", devices: this.devices, identities: this.identities };
      case "channel_link":
        return settingsQuery(this, q);
      case "transfers":
        return { kind: "transfers", transfers: this.transfers };
      case "queue":
        return { kind: "queue", queue: this.talk.queue(q.project_id) };
      case "task_artifacts": {
        const artifacts = this.work.artifacts().filter((a) =>
          (a.task_id === q.task_id || a.links?.some((l) => l.task_id === q.task_id)) &&
          (!q.before || a.artifact_id < q.before)).sort((a, b) => b.artifact_id.localeCompare(a.artifact_id));
        const page = artifacts.slice(0, Math.max(1, Math.min(q.limit, 100)));
        return { kind: "task_artifacts", history: { artifacts: page, next_before: artifacts.length > page.length ? page.at(-1)?.artifact_id ?? null : null } };
      }
      case "project_shown": {
        const tasks = new Set(this.tasks.filter((t) => t.project_id === q.project_id).map((t) => t.task_id));
        const artifacts = this.work.artifacts().filter((a) =>
          a.links?.some((l) => l.role === "shown" && tasks.has(l.task_id)) &&
          (!q.before || a.artifact_id < q.before)).sort((a, b) => b.artifact_id.localeCompare(a.artifact_id));
        const page = artifacts.slice(0, Math.max(1, Math.min(q.limit, 100)));
        return { kind: "project_shown", history: { artifacts: page, next_before: artifacts.length > page.length ? page.at(-1)?.artifact_id ?? null : null } };
      }
      case "artifact": {
        const artifact = this.work.artifacts().find(a => a.artifact_id === q.artifact_id);
        if (!artifact) throw err("not_found", "No such artifact on the simulated computer");
        return { kind: "artifact", artifact };
      }
      case "task_history": {
        const from = q.from;
        const older = this.tasks
          .filter((t) => t.project_id === q.project_id && (t.updated_at < from.updated_at || (t.updated_at === from.updated_at && t.task_id <= from.task_id)))
          .sort((a, b) => b.updated_at - a.updated_at || b.task_id.localeCompare(a.task_id));
        const page = older.slice(0, Math.max(1, q.limit));
        const next = older[page.length];
        return { kind: "task_history", records: this.#records(page), next: next ? { updated_at: next.updated_at, task_id: next.task_id } : null };
      }
      case "task": {
        const task = this.tasks.find((t) => t.task_id === q.task_id);
        if (!task) throw err("not_found", "Record not found");
        return { kind: "task", records: this.#records([task]) };
      }
      case "run": {
        const run = this.runs.find((r) => r.run_id === q.run_id);
        if (!run) throw err("not_found", "Record not found");
        return { kind: "run", run, approvals: this.approvals.filter((a) => a.run_id === run.run_id), attention: this.attention.filter((a) => a.run_id === run.run_id) };
      }
      case "approval": {
        const approval = this.approvals.find((a) => a.approval_id === q.approval_id);
        if (!approval) throw err("not_found", "Record not found");
        return { kind: "approval", approval };
      }
    }
  }

  #records(tasks: Task[]): TaskRecords {
    const ids = new Set(tasks.map((t) => t.task_id));
    return {
      tasks,
      runs: this.runs.filter((r) => ids.has(r.task_id)),
      approvals: this.approvals.filter((a) => ids.has(a.task_id)),
      attention: this.attention.filter((a) => a.task_id !== null && ids.has(a.task_id)),
    };
  }

  #list(rootId: string, dir: string): DirEntry[] {
    this.library.assertRead(rootId);
    const files = this.files[rootId] ?? {};
    const prefix = dir ? `${dir}/` : "";
    const seen = new Map<string, DirEntry>();
    for (const [path, text] of Object.entries(files)) {
      if (!path.startsWith(prefix)) continue;
      const rest = path.slice(prefix.length);
      const slash = rest.indexOf("/");
      if (slash >= 0) {
        const name = rest.slice(0, slash);
        if (!seen.has(name))
          seen.set(name, { name, hidden: name.startsWith("."), path: prefix + name, kind: "directory", size: 0, modified_at: null, title: null, description: null });
        continue;
      }
      const fm = /^---\n([\s\S]*?)\n---/.exec(text)?.[1] ?? "";
      const title = /^name:\s*(.+)$/m.exec(fm)?.[1] ?? null;
      const description = /^description:\s*(.+)$/m.exec(fm)?.[1] ?? null;
      seen.set(rest, {
        name: rest,
        hidden: rest.startsWith("."),
        path,
        kind: rest.endsWith(".md") ? "markdown" : "other",
        size: new TextEncoder().encode(text).length,
        modified_at: Date.now() - (rest.length % 7) * 3600_000,
        title,
        description,
      });
    }
    return [...seen.values()].sort((a, b) =>
      a.kind === "directory" && b.kind !== "directory" ? -1 : b.kind === "directory" && a.kind !== "directory" ? 1 : a.name.localeCompare(b.name),
    );
  }

  async #read(rootId: string, path: string): Promise<FileContent | null> {
    this.library.assertRead(rootId);
    const raw = this.files[rootId]?.[path];
    if (raw === undefined) return null;
    const text = raw.startsWith("\ufeff") ? raw.slice(1) : raw;
    return {
      root_id: rootId,
      path,
      text,
      revision: await sha256(raw),
      encoding: "utf8",
      newline: text.includes("\r\n") ? (text.replace(/\r\n/g, "").includes("\n") ? "mixed" : "cr_lf") : "lf",
      has_bom: raw.startsWith("\ufeff"),
      modified_at: this.library.modified.get(`${rootId}/${path}`) ?? Date.now() - 3600_000,
      size: new TextEncoder().encode(raw).length,
    };
  }

  // ---- Commands ----------------------------------------------------------

  /** `grants` are the sending device's, read again as a command settles. */
  async command(env: CommandEnvelope, grants: () => Grants = () => allGrants): Promise<CommandReceipt> {
    const prior = this.receipts.get(env.command_id);
    if (prior) return { ...prior, status: prior.status === "rejected" ? "rejected" : "duplicate" };
    let receipt: CommandReceipt;
    try {
      // As on the computer, a time to live counts from arrival on its clock.
      const deadline = env.expires_in_ms !== undefined ? Date.now() + env.expires_in_ms : env.expires_at;
      if (deadline !== null && deadline <= Date.now()) throw err("expired", "Command expired before admission");
      this.controlSim.before(env.body);
      if (SETTLED_LATER.has(env.body.command)) {
        receipt = { command_id: env.command_id, status: "accepted", seq: this.seq, result: null, error: null };
        this.#settleLater(env, grants);
      } else if (env.body.command === "set_conversation_access") {
        // As the computer: the choice is kept on accepting, and in force once
        // the agent has it, which takes longer while it works.
        const working = ["dispatching", "working", "awaiting_decision"].includes(this.#task(env.body.task_id).state);
        await this.#execute(env.body, env.expected_revision, grants);
        receipt = { command_id: env.command_id, status: "accepted", seq: this.seq, result: null, error: null };
        const id = env.command_id;
        this.#later(working ? ACCESS_WORKING_MS : SETTLE_MS, () => this.#progress(id, "observed"));
      } else {
        const result = await this.#execute(env.body, env.expected_revision, grants, env.command_id);
        receipt = { command_id: env.command_id, status: "accepted", seq: this.seq, result, error: null };
        this.#progress(env.command_id, "observed", result ?? undefined);
      }
    } catch (e) {
      const error = isProtocolError(e) ? e : err("internal", String(e));
      receipt = { command_id: env.command_id, status: "rejected", seq: null, result: null, error };
    }
    this.receipts.set(env.command_id, receipt);
    return receipt;
  }

  #settleLater(env: CommandEnvelope, grants: () => Grants): void {
    this.#later(env.body.command === "unlock_desktop" ? this.unlockCheckMs : SETTLE_MS, () => {
      void this.#execute(env.body, env.expected_revision, grants).then(
        (result) => this.#progress(env.command_id, "observed", result ?? undefined),
        (e: unknown) => this.#progress(env.command_id, "failed", undefined, isProtocolError(e) ? e : err("internal", String(e))),
      );
    });
  }

  #progress(commandId: string, stage: CommandStage, result?: CommandResult, error?: ProtocolError): void {
    if (result?.kind === "intervention" && result.receipt.facts.some(f => ["unknown", "still_active", "pending"].includes(f.status))) {
      stage = "reconciliation_required";
      error = err("outcome_unknown", "Stop is not confirmed; check its receipt");
    }
    this.#settled.set(commandId, { stage, result, error });
    this.emit({ type: "command_progress", command_id: commandId, stage, detail: null, result, error });
  }

  async #execute(c: Command, expectedRevision: number | null, grants: () => Grants = () => allGrants, commandId = ""): Promise<CommandResult | null> {
    if (c.command === "window_action") return { kind: "window_action", receipt: this.windows.act(c.control, c.action) };
    const workResult = this.talk.handle(c, expectedRevision) ?? this.work.handle(c, expectedRevision);
    if (workResult) return workResult.result;
    if (c.command === "rename_task" && c.expected_title !== undefined) {
      const task = this.#task(c.task_id);
      if (task.title !== c.expected_title) throw { ...err("stale_revision", `Not renamed: this task was renamed to “${task.title}” in the meantime.`), detail: { current_title: task.title } };
    } else if (["rename_task", "archive_task", "reconcile_task", "resume_task"].includes(c.command) && "task_id" in c && c.task_id) {
      const task = this.#task(c.task_id);
      if (task.revision !== expectedRevision) throw err("stale_revision", "The task changed. Review it again.");
    }
    switch (c.command) {
      case "set_contribution_root": throw err("unsupported", c.command);
      case "review_contribution": return this.contribution.review(c.input);
      case "prepare_contribution": return this.contribution.prepare(c.operation_id, c.approval, commandId);
      case "cancel_contribution": return this.contribution.cancel(c.operation_id);
      case "set_project_stage": return this.stages.change(c, expectedRevision);
      case "account": return this.accounts.act(c.action);
      case "checklist_tick":
      case "checklist_answer":
      case "checklist_ask":
      case "checklist_deliver":
      case "remove_checklist":
      case "set_file_delivery":
        return this.checklists.act(c, expectedRevision);
      case "agent_login": return this.agentLogins.act(c.action);
      case "github": return this.github.act(c.action);
      case "check_helpers":
      case "stop_child":
      case "stop_observed_session":
        if (c.command === "stop_observed_session" && c.force) throw err("unsupported", "Observed sessions cannot be force-stopped. Stop it in the agent’s terminal on the computer."); return this.children.handle(c, expectedRevision);
      case "queue_start":
      case "queue_edit_prompt":
        throw err("not_found", "Queue item not found");
      case "voice": return this.voice.command(this, c.operation);
      case "set_voice": this.voice.set(c.browser, c.desktop, expectedRevision); return null;
      case "set_agent_updates": this.agentUpdates.set(c.enabled, expectedRevision); return null;
      case "set_agent_preferences":
        if (expectedRevision !== this.agentPreferences.revision) throw err("stale_revision", "How agents work with you changed. Refresh before changing it.");
        this.agentPreferences = { ...this.agentPreferences, questions: c.questions, revision: this.agentPreferences.revision + 1 };
        return null;
      case "set_launch_defaults":
        if (expectedRevision !== this.agentPreferences.revision) throw err("stale_revision", "How agents work with you changed. Refresh before changing it.");
        this.agentPreferences = { ...this.agentPreferences, launch_defaults: c.defaults.map((d) => ({ ...d })), revision: this.agentPreferences.revision + 1 };
        return null;
      case "set_notification_settings": {
        if (expectedRevision !== this.notificationSettings.revision) throw err("stale_revision", "Notification settings changed. Refresh before changing them.");
        const projects = c.projects.filter((p) => p.mute || p.on.length || p.off.length || p.bundle !== undefined);
        this.notificationSettings = { off: [...new Set(c.off)].sort(), bundle: c.bundle, projects: structuredClone(projects), revision: this.notificationSettings.revision + 1 };
        this.emit({ type: "notification_settings_changed" });
        return null;
      }
      case "submit_report":
      case "answer_report_question":
      case "withdraw_report":
      case "set_report_settings":
        return this.reports.command(c, expectedRevision);
      case "set_report_verdict": {
        // As on the host: only a report in the owner's inbox takes a verdict.
        const a = this.attention.find((x) => x.report_id === c.report_id && x.task_id === null && x.kind === "waiting_for_you" && !x.resolved_at);
        if (!a) throw err("not_found", "That report isn't in your inbox");
        const v = c.verdict;
        const status = v.verdict === "reproduced" ? "reproduced" : v.verdict === "not_reproduced" ? "not reproduced"
          : v.verdict === "fixed" ? `fixed in ${this.host.companion_version}` : "declined";
        if (v.verdict === "declined") a.resolved_at = Date.now();
        else if (a.body) a.body = a.body.replace(/status: .*$/, `status: ${status}`);
        this.emit(a.resolved_at ? { type: "attention_resolved", attention_id: a.attention_id } : { type: "attention_upserted", item: { ...a } });
        return null;
      }
      case "look_into_report": {
        // As on the host: only a report in the owner's inbox, and choosing twice changes nothing.
        const a = this.attention.find((x) => x.report_id === c.report_id && x.task_id === null && x.kind === "waiting_for_you" && !x.resolved_at);
        if (!a) throw err("not_found", "That report isn't in your inbox");
        if (a.body && !a.body.endsWith("\nOphio is looking into it")) a.body = `${a.body}\nOphio is looking into it`;
        this.emit({ type: "attention_upserted", item: { ...a } });
        return null;
      }
      case "create_task":
        return this.#createTask(c);
      case "set_disabled_in_ophio":
        await this.disabled.set(c.kind, c.host_path, c.disabled);
        return null;
      case "create_agent": {
        const { text, ...made } = createAgent(this, c.name, c.description);
        this.emit({ type: "library_file_changed", root_id: made.file!.root_id, path: made.file!.path, revision: await sha256(text) });
        return made;
      }
      case "copy_setup_skill": {
        const { text, ...made } = await copySetupSkill(this, c.id, sha256);
        if (text !== null) this.emit({ type: "library_file_changed", root_id: made.file!.root_id, path: made.file!.path, revision: await sha256(text) });
        return made;
      }
      case "choose_setup_skill_version": {
        const text = await chooseSetupSkillVersion(this, c.id, c.keep_mine, sha256, (old) => this.libraryWorkflows.deleted(fx.ids.rootOphio, setupCopyPath(c.id)!, old));
        if (text !== null) this.emit({ type: "library_file_changed", root_id: fx.ids.rootOphio, path: setupCopyPath(c.id)!, revision: await sha256(text) });
        return null;
      }
      case "promote_one_off": {
        const p = this.projects.find((x) => x.project_id === c.project_id);
        if (!p) throw err("not_found", "No such project");
        if (!p.one_off) throw err("conflict", "This project isn’t a one-off");
        const next = { ...p, one_off: false };
        this.projects = this.projects.map((x) => (x.project_id === p.project_id ? next : x));
        this.emit({ type: "project_upserted", project: next });
        return null;
      }
      case "send_message":
        return this.#sendMessage(c.task_id, c.text, c.bundle, c.delivery), null;
      case "cancel_message": {
        const task = this.#task(c.task_id);
        const runId = task.current_run;
        const pending = runId ? this.#pendingMessages.get(runId) : undefined;
        if (!runId || pending?.get(c.message_seq) !== "queued_for_boundary") throw err("conflict", "This message is no longer held. It may already have reached the agent.");
        pending.delete(c.message_seq);
        this.#append(c.task_id, runId, { kind: "delivery", message_seq: c.message_seq, state: "cancelled" });
        return null;
      }
      case "stop_task":
        return this.#stopTask(c.task_id, c.force ?? false), null;
      case "resume_task": {
        // Mirrors the host: an unstarted or ended task gets a new run; the
        // person's text joins the timeline.
        const t = this.#task(c.task_id);
        if (!["accepted", "interrupted", "completed", "failed"].includes(t.state)) throw err("conflict", "Task has a running turn");
        if (c.read_only) {
          // One Ask turn in the task's own session: no writer, and the task is put back after.
          if (c.mode || c.carried_plan) throw err("invalid_request", "A read-only turn keeps the task's mode and carries no plan");
          if (c.model || c.effort) throw err("invalid_request", "A read-only turn uses the task's own model and effort");
          if (t.kind === "computer_use" || t.github) throw err("unsupported", "A read-only turn for this kind of task isn't supported");
          if (t.state === "accepted") throw err("conflict", "The agent can be asked once this task has stopped or finished");
          if (!this.runs.find((r) => r.run_id === t.current_run)?.native_session) throw err("conflict", "The agent's session for this task wasn't kept, so it can't be asked about it");
          const asked = this.#append(t.task_id, null, { kind: "user_message", text: c.text?.trim() ?? "", bundle: null, delivery: "host_accepted" });
          this.trackStartMessage(t.task_id, asked.seq);
          this.#sideTurns.set(t.task_id, t.state);
          this.#startRun(t, c.text ?? "", "ask");
          return null;
        }
        // Mirrors the host: a conversation carried on in Work becomes a task;
        // otherwise only a finished Plan changes mode.
        if (c.mode && !(t.conversation && c.mode === "work") && !(t.mode === "plan" && c.mode === "work" && t.state === "completed"))
          throw err("invalid_request", "Only a finished Plan can be carried out in Work mode");
        if ((c.mode ?? t.mode ?? "work") === "work" && this.projects.find((p) => p.project_id === t.project_id)?.active_writer)
          throw err("conflict", "Project already has an active or uncertain writer");
        const text = c.text?.trim() || "Continue from where you stopped.";
        if (t.state === "accepted") {
          const queue = this.talk.queue(t.project_id);
          const item = queue.items.find((i) => i.task_id === t.task_id && i.state.state !== "started");
          if (item) this.talk.canStart(queue, item, t);
        }
        if (c.text?.trim() || c.bundle) {
          const message = this.#append(t.task_id, null, { kind: "user_message", text: c.text?.trim() ?? "", bundle: c.bundle ?? null, delivery: "host_accepted", ...(c.carried_plan ? { carried_plan: c.carried_plan } : {}) });
          this.trackStartMessage(t.task_id, message.seq);
        }
        // A model chosen between turns is the task's from now on; an effort
        // alone keeps its model, and a model alone runs at its default.
        const switched = c.model ? { model: c.model, effort: c.effort ?? null } : c.effort ? { effort: c.effort } : {};
        // As on the host, a conversation turned into a task keeps its access, and may change its own project.
        const carried = c.mode && t.conversation && t.access ? { access: carriedAccess(t.access, t.project_id), access_blocked: undefined } : {};
        const changes = { ...(c.mode ? { mode: c.mode, conversation: false } : {}), ...carried, ...switched };
        const resumed = Object.keys(changes).length ? this.#touchTask(t.task_id, changes) : this.#task(t.task_id);
        this.#startRun(resumed, text);
        return null;
      }
      case "queue_continuation": {
        // The host keeps the message in Next up until its turn comes; with
        // the project free and nothing ahead of it, that is at once.
        const t = this.#task(c.task_id);
        if (!["interrupted", "completed", "failed"].includes(t.state))
          throw err("conflict", "Only a task that has stopped waits in Next up for its next message. Send it to the task instead.");
        if (expectedRevision !== null && t.revision !== expectedRevision) throw err("stale_revision", "The task changed. Review it again.");
        if (!c.text.trim() && !c.bundle) throw err("invalid_request", "Write the message first.");
        const held = (t.mode ?? "work") === "work" && this.projects.find((p) => p.project_id === t.project_id)?.active_writer;
        const waiting = held ? this.#touchTask(t.task_id, {}) : t;
        if (held) this.talk.continueLater(waiting, c.text.trim(), c.bundle ?? null, c.authorization);
        const message = this.#append(t.task_id, null, { kind: "user_message", text: c.text.trim(), bundle: c.bundle ?? null, delivery: "host_accepted" });
        this.trackStartMessage(t.task_id, message.seq);
        if (!held) this.#startRun(t, c.text.trim());
        return null;
      }
      case "rename_task":
        this.#touchTask(c.task_id, { title: c.title });
        return null;
      case "set_conversation_access": {
        const held = this.#task(c.task_id);
        if (!held.conversation && !held.access) throw err("invalid_request", ONLY_CONVERSATIONS);
        // As on the host: what it was kept from may be let now; it asks again if not.
        this.#touchTask(c.task_id, { access: normalizedAccess(c.access), access_blocked: undefined });
        return null;
      }
      case "archive_task": {
        // As on the host: kept so Undo can bring it back to where it was.
        const was = this.#task(c.task_id).state;
        if (was !== "archived") this.#archivedFrom.set(c.task_id, was);
        const archived = this.#touchTask(c.task_id, { state: "archived" });
        // As on the host: an archived task never starts, so it leaves Next up.
        const queue = this.talk.queue(archived.project_id);
        if (queue.items.some((i) => i.task_id === c.task_id && i.state.state !== "started")) {
          const removed = queue.items.filter((i) => i.task_id === c.task_id && i.state.state !== "started").map((i) => i.item_id);
          queue.items = queue.items.filter((i) => i.task_id !== c.task_id || i.state.state === "started");
          this.talk.withdraw(queue, removed); this.talk.refresh(queue); this.talk.put(queue);
        }
        return null;
      }
      case "unarchive_task": {
        if (this.#task(c.task_id).state !== "archived") throw err("conflict", "This task isn't archived");
        // Only a task that had ended comes back, as on the host.
        const was = this.#archivedFrom.get(c.task_id);
        if (!was || !["completed", "failed", "interrupted", "reconciliation_required"].includes(was))
          throw err("conflict", "This task can't be brought back; it stays in Archived to read");
        this.#archivedFrom.delete(c.task_id);
        this.#touchTask(c.task_id, { state: was });
        return null;
      }
      case "reconcile_task": {
        const task = this.#task(c.task_id);
        if (task.state !== "reconciliation_required") throw err("conflict", "This task does not need reconciliation.");
        // Mirrors the host: retry is refused; the person marks the old run, then resumes.
        if (c.resolution === "retry_as_new_run")
          throw err("conflict", "First reconcile the old run as completed or failed, then explicitly resume");
        // As on the host: the old run ends with the recorded result and frees the project's writer.
        const run = task.current_run ? this.runs.find((r) => r.run_id === task.current_run) : undefined;
        if (run) {
          this.#touchRun(run.run_id, { human_resolution: { resolution: c.resolution, at: Date.now(), by: { kind: "device", device_id: fx.ids.device } }, state: c.resolution === "mark_failed" ? "failed" : "completed", ended_at: run.ended_at ?? Date.now(), holds_writer: false });
          try {
            const saved = JSON.parse(localStorage.getItem("sim.human-decisions") ?? "{}");
            saved[run.run_id] = this.#run(run.run_id).human_resolution;
            localStorage.setItem("sim.human-decisions", JSON.stringify(saved));
          } catch { /* The live fixture still retains the decision. */ }
          if ((task.mode ?? "work") === "work") this.#releaseWriter(task.project_id, run.run_id);
        }
        for (const item of this.attention.filter(a => a.task_id === c.task_id && (a.run_id === run?.run_id || (run && a.run_id === null && a.created_at >= run.admitted_at)) && a.kind === "reconciliation_required" && !a.resolved_at)) {
          item.resolved_at = Date.now();
          this.emit({ type: "attention_resolved", attention_id: item.attention_id });
        }
        this.#touchTask(c.task_id, { state: c.resolution === "mark_failed" ? "failed" : "completed" });
        return null;
      }
      case "resolve_approval":
        this.#resolveApproval(c.approval_id, c.decision, expectedRevision);
        return null;
      case "acknowledge_attention": {
        const a = this.attention.find((x) => x.attention_id === c.attention_id);
        if (!a) throw err("not_found", "That item is gone");
        if (a.resolved_at) throw err("already_resolved", "Already cleared");
        a.resolved_at = Date.now();
        this.emit({ type: "attention_resolved", attention_id: a.attention_id });
        if (a.task_id) this.#recountAttention(a.task_id);
        return null;
      }
      case "queue_move":
      case "queue_remove":
      case "queue_set_authorization":
      case "queue_set_dependencies":
        return this.#queueEdit(c), null;
      case "queue_pause":
      case "queue_resume": {
        this.queue = { ...this.queue, paused: c.command === "queue_pause", revision: this.queue.revision + 1 };
        this.emit({ type: "queue_changed", queue: this.queue });
        if (c.command === "queue_pause")
          this.#intervention(null, null, "pause_queue", [
            { label: "No new task will start from Next up", status: "done", detail: null },
            { label: "Current task keeps running", status: "still_active", detail: "Use Stop task to end it" },
          ]);
        return null;
      }
      case "request_human_lease":
        return { kind: "lease_granted", lease: this.#grantHuman(c.target_id, c.geometry_revision) };
      case "release_lease":
        this.#releaseLease(c.lease_id);
        return null;
      case "pause_input":
        this.#pauseInput(c.target_id);
        return null;
      case "take_over":
        return { kind: "lease_granted", lease: await this.#takeOver(c.target_id, c.geometry_revision) };
      case "return_to_agent":
        this.#returnToAgent(c.target_id, c.run_id);
        return null;
      case "start_private_step":
      case "end_private_step":
        this.#privateStep(c.target_id, c.command === "start_private_step");
        return null;
      case "withdraw_observation": {
        const v = this.#control(c.target_id);
        if (!v.model_observation) throw err("not_found", "No model observation on this target");
        this.#setControl({ ...v, model_observation: { ...v.model_observation, state: "withdrawal_pending" } });
        this.#later(700, () => {
          const cur = this.#control(c.target_id);
          if (cur.model_observation) this.#setControl({ ...cur, model_observation: { ...cur.model_observation, state: "withdrawn" } });
        });
        return null;
      }
      case "lock_desktop":
        this.#intervention(null, null, "lock_desktop", [
          { label: "Lock requested", status: "done", detail: null },
          { label: "Session locked", status: "unknown", detail: "Hyprland did not report lock state" },
        ]);
        return null;
      case "unlock_desktop": {
        if (!c.password) throw err("invalid_request", "Type the computer’s password");
        if (new TextEncoder().encode(c.password).byteLength > 512) throw err("invalid_request", "That is longer than any computer’s password");
        if (this.host.capabilities.unlock_session?.state !== "available") throw err("unsupported", "Windows can’t be unlocked from Ophio");
        // As on the computer: three tries that didn't unlock it pause unlocking
        // until the first is 15 minutes old.
        const now = Date.now();
        this.#unlockTries = this.#unlockTries.filter((at) => now - at < UNLOCK_WINDOW_MS);
        const first = this.#unlockTries[0];
        if (first !== undefined && this.#unlockTries.length >= UNLOCK_TRIES) {
          const minutes = Math.max(1, Math.ceil((UNLOCK_WINDOW_MS - (now - first)) / 60_000));
          throw err("limit_exceeded", `Unlocking from Ophio is paused for ${minutes} min after three tries that didn’t unlock it. Unlock it at the computer.`);
        }
        const locked = this.control.filter((v) => v.desktop_lock === "locked");
        if (!locked.length) throw err("conflict", "The computer is already unlocked");
        const { control_input, admin } = grants();
        if (!control_input || !admin) throw err("permission_denied", "This device or chat wasn’t given permission for that");
        // A right password isn't counted, but the wrong ones before it still are.
        if (c.password !== SIM_DESKTOP_PASSWORD) {
          this.#unlockTries.push(now);
          throw err("permission_denied", "That isn’t the computer’s password");
        }
        for (const v of locked) this.controlSim.lock(false, v.target_id);
        return null;
      }
      case "capture_target":
        return captureTarget(this, c.target_id);
      case "delete_artifact": {
        if (!this.artifactBytes.has(c.artifact_id)) throw err("not_found", "That file is no longer on the computer");
        // As on the computer: what was sent stays as the record of the message.
        const sent = this.journal.some(({ event: e }) =>
          (e.type === "timeline_appended" && e.item.item.kind === "user_message" && e.item.item.bundle?.sources.some((src) => src.artifact_id === c.artifact_id))
          || (e.type === "artifact_created" && e.artifact.artifact_id === c.artifact_id && e.artifact.links?.some((l) => l.role === "source")));
        if (sent) throw err("conflict", "This picture is part of a message to an agent, so it stays with that message");
        this.artifactBytes.delete(c.artifact_id);
        this.emit({ type: "artifact_deleted", artifact_id: c.artifact_id });
        return null;
      }
      case "library_save": {
        this.library.assertWrite(c.root_id);
        const file = await this.#read(c.root_id, c.path);
        if (!file) return { kind: "saved", outcome: { outcome: "missing" } };
        if (file.revision !== c.base_revision) return { kind: "saved", outcome: { outcome: "conflict", current: file } };
        const normalized = c.newline === "cr_lf" ? c.text.replace(/\r\n/g, "\n").replace(/\n/g, "\r\n") : c.newline === "lf" ? c.text.replace(/\r\n/g, "\n") : c.text;
        const text = (c.bom ? "\ufeff" : "") + normalized;
        this.files[c.root_id]![c.path] = text;
        this.library.modified.set(`${c.root_id}/${c.path}`, Date.now());
        const revision = await sha256(text);
        this.emit({ type: "library_file_changed", root_id: c.root_id, path: c.path, revision });
        return { kind: "saved", outcome: this.library.consumeUncertain() ? { outcome: "uncertain", recovery_id: this.libraryCompletion.retain(file, text) } : { outcome: "saved", revision } };
      }
      case "library_create": {
        this.library.assertWrite(c.root_id);
        const files = this.files[c.root_id];
        if (!files) throw err("not_found", "No such folder");
        if (files[c.path] !== undefined) throw err("conflict", "A file with that name already exists");
        files[c.path] = c.text;
        const revision = await sha256(c.text);
        this.emit({ type: "library_file_changed", root_id: c.root_id, path: c.path, revision });
        return { kind: "created", path: c.path, revision };
      }
      case "library_rename": {
        this.library.assertWrite(c.root_id);
        const files = this.files[c.root_id];
        const text = files?.[c.from];
        if (!files || text === undefined) throw err("not_found", "That file is no longer there");
        if ((await sha256(text)) !== c.base_revision) throw err("stale_revision", "The file changed on the computer");
        if (files[c.to] !== undefined) throw err("conflict", "A file with that name already exists");
        delete files[c.from];
        files[c.to] = text;
        this.emit({ type: "library_file_changed", root_id: c.root_id, path: c.from, revision: null });
        this.emit({ type: "library_file_changed", root_id: c.root_id, path: c.to, revision: await sha256(text) });
        return null;
      }
      case "library_delete": {
        this.library.assertWrite(c.root_id);
        const files = this.files[c.root_id];
        const text = files?.[c.path];
        if (!files || text === undefined) throw err("not_found", "That file is no longer there");
        if ((await sha256(text)) !== c.base_revision) throw err("stale_revision", "The file changed on the computer");
        delete files[c.path];
        this.emit({ type: "library_file_changed", root_id: c.root_id, path: c.path, revision: null });
        return this.libraryWorkflows.deleted(c.root_id, c.path, text);
      }
      case "library_recovery_discard":
        return this.libraryCompletion.discard(c.root_id, c.recovery_id);
      case "library_restore":
      case "create_reusable_draft":
        return this.libraryWorkflows.command(c);
      case "make_reusable": {
        const root = this.roots.find((r) => r.kind === (c.kind === "agent" ? "agents" : c.kind === "note" ? "notes" : "skills"));
        if (!root) throw err("unsupported", "No folder is set up for that");
        const slug = c.name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "untitled";
        const path = c.kind === "skill" ? `${slug}/SKILL.md` : `${slug}.md`;
        if (this.files[root.root_id]?.[path] !== undefined) throw err("conflict", "A draft with that name already exists.");
        const text = this.work.reusableDraft(c.task_id, c.name, slug);
        this.files[root.root_id]![path] = text;
        this.emit({ type: "library_file_changed", root_id: root.root_id, path, revision: await sha256(text) });
        return { kind: "draft_created", root_id: root.root_id, path };
      }
      case "open_terminal": {
        const project = this.projects.find((p) => p.project_id === c.project_id);
        const term: TerminalInfo = {
          terminal_id: uuidv7(Date.now()),
          title: `${project?.name ?? "home"} — ${c.harness ? harnessLabel(c.harness) : "zsh"}`,
          cwd: c.cwd ?? project?.host_path ?? "/home/sam",
          project_id: c.project_id,
          harness: c.harness,
          cols: c.cols,
          rows: c.rows,
          state: { state: "running" },
          created_at: Date.now(),
          output_offset: 0,
        };
        this.terminals = [...this.terminals, term];
        this.emit({ type: "terminals_changed", terminals: this.terminals });
        return { kind: "terminal_opened", terminal: term };
      }
      case "close_terminal":
        this.terminals = this.terminals.map((t) =>
          t.terminal_id === c.terminal_id ? { ...t, state: { state: "exited", code: 0 } } : t,
        );
        this.emit({ type: "terminals_changed", terminals: this.terminals });
        return null;
      case "create_upload_reviewed":
      case "create_upload": {
        const taskId = c.command === "create_upload_reviewed" ? c.task_id : undefined;
        // As on the computer: only the agent's show marks a picture as shown.
        if (c.command === "create_upload" && c.evidence?.role === "shown") throw err("invalid_request", "Only the agent shows a picture or video");
        if (taskId) {
          this.#task(taskId);
          if (c.destination.kind !== "artifact") throw err("invalid_request", "Task evidence uploads must use artifact storage");
        }
        const into = c.destination;
        if (into.kind === "project_dir" && this.projects.some((p) => p.project_id === into.project_id && p.home)) throw err("path_not_allowed", HOME_FILES);
        const tr: TransferRecord = {
          transfer_id: uuidv7(Date.now()),
          direction: "phone_to_host",
          name: c.command === "create_upload_reviewed" && c.collision.choice === "keep_both" ? c.collision.name : c.name,
          size: c.size,
          sha256: c.sha256,
          transferred: 0,
          state: { state: "pending" },
          destination: c.destination,
          destination_folder: c.destination.kind === "inbox" ? this.host.inbox_path : undefined,
          host_path: null,
          created_at: Date.now(),
          updated_at: Date.now(),
        };
        if (taskId) this.uploadTasks.set(tr.transfer_id, taskId);
        if (c.command === "create_upload_reviewed") this.libraryWorkflows.collisions.set(tr.transfer_id, c.collision);
        if (c.command === "create_upload" && c.evidence) this.uploadEvidence.set(tr.transfer_id, c.evidence);
        this.transfers = [tr, ...this.transfers];
        this.emit({ type: "transfer_upserted", transfer: tr });
        return { kind: "transfer", transfer: tr };
      }
      case "create_download": {
        const src = await this.#downloadSource(c.source);
        const tr: TransferRecord = {
          transfer_id: uuidv7(Date.now()),
          direction: "host_to_phone",
          shown_in_task: c.shown_in_task,
          name: src.name,
          size: src.bytes.length,
          sha256: await sha256(src.bytes),
          transferred: 0,
          state: { state: "pending" },
          destination: null,
          host_path: src.hostPath,
          created_at: Date.now(),
          updated_at: Date.now(),
        };
        this.#downloads.set(tr.transfer_id, { bytes: src.bytes, sha256: tr.sha256 ?? "" });
        this.transfers = [tr, ...this.transfers];
        this.emit({ type: "transfer_upserted", transfer: tr });
        return { kind: "transfer", transfer: tr };
      }
      case "cancel_transfer": {
        const tr = this.transfers.find((t) => t.transfer_id === c.transfer_id);
        if (!tr) throw err("not_found", "That transfer isn’t on the computer");
        if (tr.state.state === "completed" || tr.state.state === "verifying") throw err("already_resolved", "Transfer is already publishing or completed");
        this.#downloads.delete(tr.transfer_id);
        this.uploadStaging.remove(tr.transfer_id);
        this.#setTransfer({ ...tr, state: { state: "cancelled" }, updated_at: Date.now() });
        return null;
      }
      case "register_preview":
      case "remove_preview":
      case "check_preview":
      case "rename_device":
      case "revoke_device":
      case "set_device_grants":
      case "change_device_grants":
      case "approve_channel_link":
      case "refuse_channel_link":
      case "revoke_channel_identity":
      case "set_channel_project":
        return settingsCommand(this, c);
      case "run_readiness_check":
        this.readiness = this.tablet.refresh(this.#readiness(Date.now()));
        this.emit({ type: "harnesses_changed", harnesses: this.harnesses });
        this.emit({ type: "readiness_reported", report: this.readiness });
        return null;
      case "add_project": {
        if (this.projects.some(p => p.host_path === c.host_path)) throw err("conflict", "Project is already registered");
        if (["/", "/home", "/home/sam"].includes(c.host_path)) throw err("path_not_allowed", "Choose the project’s own folder.");
        const p: Project = {
          project_id: uuidv7(Date.now()),
          name: c.name ?? c.host_path.split("/").filter(Boolean).pop() ?? "project",
          host_path: c.host_path,
          default_harness: null,
          default_model: null,
          default_effort: null,
          active_writer: null,
          vcs: this.github.isRepoFolder(c.host_path) ? { kind: "git", branch: "main", head: "3b7e0c1", dirty_files: 0 } : null,
          created_at: Date.now(),
        };
        this.projects = [...this.projects, p];
        this.emit({ type: "project_upserted", project: p });
        return { kind: "project_added", project_id: p.project_id };
      }
      case "update_project_defaults": {
        const p = this.projects.find((x) => x.project_id === c.project_id);
        if (!p) throw err("not_found", "No such project");
        const next = { ...p, default_harness: c.harness, default_model: c.model, default_effort: c.effort };
        this.projects = this.projects.map((x) => (x.project_id === p.project_id ? next : x));
        this.emit({ type: "project_upserted", project: next });
        return null;
      }
      case "remove_project":
        this.projects = this.projects.filter((x) => x.project_id !== c.project_id);
        this.emit({ type: "project_removed", project_id: c.project_id });
        return null;
      case "delete_one_off": {
        // As on the host: refused while anything of its tasks still runs,
        // then forgotten, its tasks archived and its folder deleted.
        const p = this.projects.find((x) => x.project_id === c.project_id);
        if (!p) throw err("not_found", "No such project");
        if (!p.one_off) throw err("conflict", "This project isn’t a one-off");
        const tasks = this.tasks.filter((t) => t.project_id === p.project_id);
        const running = tasks.some((t) => {
          const run = t.current_run ? this.runs.find((r) => r.run_id === t.current_run) : undefined;
          return run && (!isTerminalRunState(run.state) || stillFinishing(run));
        });
        if (running) throw err("conflict", "Finish or reconcile active runs, then delete the one-off");
        this.projects = this.projects.filter((x) => x.project_id !== p.project_id);
        this.emit({ type: "project_removed", project_id: p.project_id });
        for (const t of tasks) if (t.state !== "archived") this.#touchTask(t.task_id, { state: "archived" });
        return null;
      }
    }
  }

  // ---- Tasks and runs ----------------------------------------------------

  #task(id: string): Task {
    const t = this.tasks.find((x) => x.task_id === id);
    if (!t) throw err("not_found", "That task no longer exists");
    return t;
  }

  #recountAttention(id: string): void {
    const task = this.#task(id);
    const count = blockingAttentionCount(this.attention, id);
    if (task.attention_count === count) return;
    task.attention_count = count;
    this.emit({ type: "task_upserted", task });
  }

  /** A command the computer gives itself, as Run all does: past the phone's grants and checks. */
  inside(c: Command, expectedRevision: number | null = null): Promise<CommandResult | null> {
    return this.#execute(c, expectedRevision);
  }

  /** A change the computer made to a task, told like any other. */
  touchTask(id: string, patch: Partial<Task>): Task {
    return this.#touchTask(id, patch);
  }

  /** `resolve_task_attention`: what a task raised is answered. */
  resolveTaskAttention(id: string): void {
    for (const a of this.attention) {
      if (a.task_id !== id || a.resolved_at) continue;
      a.resolved_at = Date.now();
      this.emit({ type: "attention_resolved", attention_id: a.attention_id });
    }
    this.#recountAttention(id);
  }

  /** The state each archived task was archived from, for Undo. */
  #archivedFrom = new Map<string, TaskState>();

  #touchTask(id: string, patch: Partial<Task>): Task {
    const t = this.#task(id);
    const next: Task = { ...t, ...patch, attention_count: blockingAttentionCount(this.attention, id), revision: t.revision + 1, updated_at: Date.now() };
    this.tasks = this.tasks.map((x) => (x.task_id === id ? next : x));
    this.emit({ type: "task_upserted", task: next });
    return next;
  }

  #run(id: string): Run {
    const r = this.runs.find((x) => x.run_id === id);
    if (!r) throw err("not_found", "No such run");
    return r;
  }

  #touchRun(id: string, patch: Partial<Run>): Run {
    const next = { ...this.#run(id), ...patch };
    this.runs = this.runs.map((x) => (x.run_id === id ? next : x));
    this.emit({ type: "run_upserted", run: next }, HARNESS_SOURCE);
    return next;
  }

  async #createTask(c: Extract<Command, { command: "create_task" }>): Promise<CommandResult> {
    validateComputerStart(this, c);
    if ((c.start || c.place) && c.kind !== "coding") throw err("unsupported", "An agent, skill or one-off for this kind of task");
    if (c.place && c.placement.mode !== "start_now") throw err("invalid_request", "A task on its own starts now; it has no Next up to wait in");
    const now0 = Date.now();
    const placed = c.place ? oneOff(this, this.projects, c.project_id, c.place, now0) : null;
    const project = placed ?? this.projects.find((p) => p.project_id === c.project_id);
    if (!project) throw err("not_found", "That project is no longer on this computer");
    if (!c.prompt.trim()) throw err("invalid_request", "Say what you want done");
    if (c.access && !c.conversation) throw err("invalid_request", ONLY_CONVERSATIONS);
    const linked = this.github.prepare(c);
    const start = c.start?.type === "same_as" ? sameStart(this.tasks, c.project_id, c.start.task_id, !!c.place)
      : c.start ? await resolveStart(this, c.start, sha256, this.disabled) : undefined;
    const inheritedPath = c.start?.type === "same_as" ? this.#startPaths.get(c.start.task_id) : undefined;
    if (inheritedPath && start && start.type !== "scratch") this.disabled.refuse(inheritedPath, start.name);
    const chosen = c.start;
    const chosenPath = chosen && (chosen.type === "agent" || chosen.type === "skill")
      ? (await catalog(this, sha256)).entries.find((e) => e.kind === chosen.type && e.source === chosen.source && e.id === chosen.id)?.host_path
      : inheritedPath;
    const branch = githubTaskBranch(linked, c.branch);
    const harness = this.harnesses.find((h) => h.kind === c.harness);
    if (c.kind === "computer_use" && harness?.capabilities.computer_use_delegation.state !== "available") throw err("unsupported", "This agent can’t use the computer");
    if (!harness || harness.capabilities.start_run.state !== "available")
      throw err("unsupported", `${harness?.display_name ?? c.harness} can only be used in a terminal`);
    // Mirrors domain::task_mode: a mode runs only if the harness enforces it.
    const mode = linked?.plan ? "plan" : c.mode ?? "work";
    if (c.target_id !== null && mode !== "work") throw err("invalid_request", "Computer-use tasks run in Work mode");
    const support = harness.modes?.find((m) => m.mode === mode)?.availability;
    if (support ? support.state !== "available" : mode !== "work")
      throw err("unsupported", support && "reason" in support ? support.reason : "The harness did not declare enforcement for this task mode");
    // Only Work takes the project's writer; Ask and Plan read beside it.
    if (c.placement.mode === "start_now" && mode === "work" && project.active_writer)
      throw err("conflict", "Project already has an active or uncertain writer");
    if (placed) {
      this.projects = [...this.projects, placed];
      this.emit({ type: "project_upserted", project: placed });
    }
    // Mirrors CreateTask: the project's own model and effort fill in only
    // for its usual agent, and only when the person didn't choose.
    const fill = project.default_harness === c.harness && c.agent_chosen !== true;
    const now = Date.now();
    const task: Task = {
      task_id: uuidv7(now),
      project_id: c.project_id,
      kind: c.kind,
      mode,
      title: c.title ?? linked?.title ?? titleFrom(c.prompt),
      state: "accepted",
      revision: 1,
      harness: c.harness,
      model: c.model ?? (fill ? project.default_model : null) ?? harness.models.find((m) => m.is_default)?.id ?? null,
      effort: c.effort ?? (fill ? project.default_effort : null),
      launch_policy: c.launch_policy,
      ...(branch ? { branch } : {}),
      ...(linked ? { github: linked.github } : {}),
      ...(start ? { start } : {}),
      created_by: { kind: "device", device_id: fx.ids.device },
      created_at: now,
      updated_at: now,
      current_run: null,
      target_id: c.target_id,
      mission: c.mission,
      attention_count: 0,
      status_line: null,
      ...(c.conversation ? { conversation: true } : {}),
      ...(c.access ? { access: normalizedAccess(c.access) } : {}),
      // Mirrors CreateTask: its part in a review is saved with it.
      ...(c.review ? { review: { role: c.review } } : {}),
    };
    if (chosenPath) this.#startPaths.set(task.task_id, chosenPath);
    this.tasks = [task, ...this.tasks];
    this.emit({ type: "task_upserted", task }, DEVICE);
    const message = this.#append(task.task_id, null, { kind: "user_message", text: c.prompt, bundle: c.bundle, delivery: "host_accepted" });
    this.trackStartMessage(task.task_id, message.seq);
    let queueItem: string | null = null;
    if (c.placement.mode === "queue") {
      // The prompt's append moved the task on; its go-ahead names the current revision.
      queueItem = this.talk.enqueue(this.#task(task.task_id), c.prompt, c.bundle, c.placement);
      if (c.placement.authorization === "auto_start") this.talk.advance(task.project_id);
    } else {
      this.#startRun(task, c.prompt);
      // Mirrors CreateTask: a finder whose fixes start by themselves resumes a paused Next up.
      const s = this.github.settings.get(task.project_id);
      if (linked?.github.link.type === "find" && s && s.mode !== "off" && s.workflow.fix_findings === "by_itself" && this.queue.project_id === task.project_id && this.queue.paused) {
        this.queue = { ...this.queue, paused: false, pause_reason: undefined, revision: this.queue.revision + 1 };
        this.emit({ type: "queue_changed", queue: this.queue });
      }
    }
    return { kind: "task_created", task_id: task.task_id, queue_item_id: queueItem };
  }

  /** Tasks answering a read-only question, and the state each goes back to. */
  #sideTurns = new Map<string, Task["state"]>();
  #ended(taskId: string, state: Task["state"]): Task["state"] {
    const before = this.#sideTurns.get(taskId);
    this.#sideTurns.delete(taskId);
    return before ?? state;
  }

  #startRun(task: Task, prompt: string, mode: TaskMode = task.mode ?? "work"): void {
    const now = Date.now();
    const run: Run = {
      force_stoppable: true,
      run_id: uuidv7(now),
      task_id: task.task_id,
      harness: task.harness,
      requested_model: task.model,
      actual_model: null,
      requested_effort: task.effort,
      launch_policy: task.launch_policy,
      actual_effort: null,
      account_label: "Personal",
      native_session: null,
      state: "dispatching",
      admitted_at: now,
      dispatched_at: null,
      acknowledged_at: null,
      ended_at: null,
      children: [],
      outcome: null,
      holds_writer: mode === "work",
      mode,
    };
    this.runs = [...this.runs, run];
    this.emit({ type: "run_upserted", run });
    const messageSeq = this.#queuedStartMessages.get(task.task_id);
    if (messageSeq !== undefined) {
      this.#startMessages.set(run.run_id, messageSeq);
      this.#queuedStartMessages.delete(task.task_id);
    }
    this.talk.started(task, run.run_id);
    if (run.holds_writer) this.#setWriter(task.project_id, run.run_id);
    this.#touchTask(task.task_id, { state: "dispatching", current_run: run.run_id, status_line: "Starting" });
    if (task.kind === "computer_use") { this.computer.start(task, run); this.#confirmStartMessage(task.task_id, run.run_id); return; }
    this.#later(500, () => {
      if (!this.#isLive(run.run_id)) return;
      this.#confirmStartMessage(task.task_id, run.run_id);
      this.#touchRun(run.run_id, {
        state: "working",
        dispatched_at: Date.now() - 300,
        acknowledged_at: Date.now(),
        actual_model: fx.reportedModel(task.model),
        actual_effort: task.effort,
        native_session: run.run_id.slice(-8),
      });
      this.#append(task.task_id, run.run_id, { kind: "run_started", run_id: run.run_id });
      this.#touchTask(task.task_id, { state: "working", status_line: "Reading the project" });
      this.#script(task.task_id, run.run_id, mode === "work" ? replyFor(prompt) : mode !== (task.mode ?? "work") ? summaryReply() : readOnlyReply(mode));
    });
  }

  #confirmStartMessage(taskId: string, runId: string): void {
    const message_seq = this.#startMessages.get(runId);
    if (message_seq !== undefined) {
      this.#append(taskId, runId, { kind: "delivery", message_seq, state: "native_delivered" });
      this.#startMessages.delete(runId);
    }
  }

  /** Stream a scripted reply: activity, streamed text, then completion. */
  #script(taskId: string, runId: string, reply: ScriptedReply, done: "complete" | "await" = "complete"): void {
    let t = 300;
    for (const a of reply.activity) {
      this.#later(t, () => {
        if (!this.#isLive(runId)) return;
        this.live({ kind: "activity_status", task_id: taskId, run_id: runId, text: a.summary });
        this.#append(taskId, runId, { kind: "activity", tool: a.tool, summary: a.summary, status: "succeeded", detail: a.detail ?? null });
      });
      t += 900;
    }
    const key = uuidv7(Date.now());
    const words = reply.text.split(/(?<=\s)/);
    let acc = "";
    for (let i = 0; i < words.length; i += 3) {
      const chunk = words.slice(i, i + 3).join("");
      this.#later(t, () => {
        if (!this.#isLive(runId)) return;
        acc += chunk;
        this.live({ kind: "agent_text_delta", task_id: taskId, run_id: runId, message_key: key, text: chunk });
      });
      t += 70;
    }
    this.#later(t + 200, () => {
      if (!this.#isLive(runId)) return;
      this.#append(taskId, runId, { kind: "agent_message", text: reply.text, partial: false });
      if (done === "await") return;
      this.#finishRun(taskId, runId, reply);
    });
  }

  #isLive(runId: string): boolean {
    const r = this.runs.find((x) => x.run_id === runId);
    return !!r && !isTerminalRun(r.state) && r.state !== "stopping";
  }

  #finishRun(taskId: string, runId: string, reply: ScriptedReply): void {
    this.#failQueuedMessages(taskId, runId);
    const run = this.#touchRun(runId, {
      state: "completed",
      ended_at: Date.now(),
      outcome: {
        summary: reply.summary ?? reply.text.split("\n")[0]!,
        changed_files: reply.files ?? [],
        checks: reply.checks ?? [],
        still_active: [],
        unknowns: reply.unknowns ?? [],
        usage: { input_tokens: 48_200, output_tokens: 2_310, duration_ms: 42_000, reported_cost: null },
      },
      holds_writer: false,
    });
    this.#append(taskId, runId, { kind: "run_ended", run_id: runId, state: "completed" });
    const task = this.#touchTask(taskId, { state: this.#ended(taskId, "completed"), status_line: null });
    this.#releaseWriter(task.project_id, runId);
    const item: AttentionItem = {
      attention_id: uuidv7(Date.now()),
      kind: "completed",
      title: task.title,
      body: null,
      task_id: taskId,
      run_id: run.run_id,
      approval_id: null,
      created_at: Date.now(),
      resolved_at: null,
      priority: 5,
    };
    this.attention = [...this.attention, item];
    this.emit({ type: "attention_upserted", item });
  }

  #setWriter(projectId: string, runId: string | null): void {
    const p = this.projects.find((x) => x.project_id === projectId);
    if (!p || p.active_writer === runId) return;
    const next = { ...p, active_writer: runId };
    this.projects = this.projects.map((x) => (x.project_id === projectId ? next : x));
    this.emit({ type: "project_upserted", project: next });
  }

  /** A read-only run ending never frees another run's writer. */
  #releaseWriter(projectId: string, runId: string): void {
    if (this.projects.find((x) => x.project_id === projectId)?.active_writer === runId) this.#setWriter(projectId, null);
    this.talk.released(projectId, runId);
  }

  #sendMessage(taskId: string, text: string, bundle: AnnotationBundle | null, delivery: Extract<Command, { command: "send_message" }>["delivery"]): void {
    const task = this.#task(taskId);
    // As on the computer: an annotation alone is a complete message.
    if (!text.trim() && !bundle) throw err("invalid_request", "A message requires text or an annotation");
    const run = task.current_run ? this.runs.find((r) => r.run_id === task.current_run) : undefined;
    // The host only accepts messages for a running turn; ended tasks use resume_task.
    if (!(task.state === "working" || task.state === "awaiting_decision") || !run || isTerminalRun(run.state))
      throw err("conflict", "Task has no running turn");
    const active = true;
    if (active && run.state === "awaiting_approval" && delivery === "steer")
      throw err("conflict", "Answer the waiting approval first, or stop the task");
    const harness = fx.harnesses.find((h) => h.kind === task.harness)!;
    if (active && delivery === "steer" && harness.capabilities.steer_running.state !== "available")
      throw err("unsupported", "Codex takes new text at its next turn. Choose “After this turn”.");
    const item = this.#append(taskId, run?.run_id ?? null, {
      kind: "user_message",
      text,
      bundle,
      delivery: delivery === "next_turn" ? "queued_for_boundary" : "host_accepted",
    });
    const pending = this.#pendingMessages.get(run.run_id) ?? new Map<number, "queued_for_boundary" | "host_accepted">();
    pending.set(item.seq, delivery === "next_turn" ? "queued_for_boundary" : "host_accepted");
    this.#pendingMessages.set(run.run_id, pending);
    const deliver = () => {
      if (!this.#isLive(run.run_id) || !pending.has(item.seq)) return;
      // As on the computer: a waiting approval is part of the turn, so a
      // message held for the turn's end waits until it is answered.
      if (pending.get(item.seq) === "queued_for_boundary" && this.runs.find((r) => r.run_id === run.run_id)?.state === "awaiting_approval") {
        this.#later(600, deliver);
        return;
      }
      if (pending.get(item.seq) === "queued_for_boundary") {
        this.#append(taskId, run.run_id, { kind: "delivery", message_seq: item.seq, state: "host_accepted" });
      }
      this.#append(taskId, run.run_id, { kind: "delivery", message_seq: item.seq, state: "native_delivered" });
      pending.delete(item.seq);
      if (!pending.size) this.#pendingMessages.delete(run.run_id);
      this.#touchTask(taskId, { status_line: "Adjusting to your message" });
      this.#script(taskId, run.run_id, steerReply(text), "await");
      this.#later(4000, () => {
        if (this.#isLive(run.run_id)) this.#touchTask(taskId, { status_line: "Working" });
      });
    };
    this.#later(600, deliver);
  }

  #failQueuedMessages(taskId: string, runId: string): void {
    const message_seq = this.#startMessages.get(runId);
    if (message_seq !== undefined) {
      this.#append(taskId, runId, { kind: "delivery", message_seq, state: "unknown" });
      this.#startMessages.delete(runId);
    }
    for (const [message_seq, state] of this.#pendingMessages.get(runId) ?? []) {
      if (state === "queued_for_boundary") this.#append(taskId, runId, { kind: "delivery", message_seq, state: "failed" });
    }
    this.#pendingMessages.delete(runId);
  }

  /** As on the computer: Stop withdraws every held message at once, and none is sent later. */
  #withdrawHeldMessages(taskId: string, runId: string): void {
    const pending = this.#pendingMessages.get(runId);
    for (const [message_seq, state] of pending ?? []) {
      if (state !== "queued_for_boundary") continue;
      pending!.delete(message_seq);
      this.#append(taskId, runId, { kind: "delivery", message_seq, state: "cancelled", withdrawn_by_stop: true });
    }
  }

  #stopCounts = new Map<string, number>();

  #stopTask(taskId: string, force = false): void {
    const task = this.#task(taskId);
    const run = task.current_run ? this.runs.find((r) => r.run_id === task.current_run) : undefined;
    const ended = !run || (isTerminalRun(run.state) && !stillFinishing(run));
    // Mirrors the host: between turns, a watching task stops watching.
    if (ended && task.wake) {
      const text = task.wake.answer
        ? "Stopped waiting. The other agent's answer stays in its own task, and Ophio won't wake the agent."
        : "Stopped watching. Ophio won't wake the agent.";
      this.#touchTask(taskId, { wake: undefined });
      this.#append(taskId, null, { kind: "note", text });
      return;
    }
    // An ended turn stays stoppable while something it started may still run.
    if (ended) throw err("conflict", "This task is not running");
    if (force && !run.force_stoppable) throw err("unsupported", `Force stop is only available for runs ${PRODUCT_NAME} started. Stop this session in the agent’s terminal on the computer.`);
    const queue = this.talk.queue(task.project_id);
    queue.paused = true;
    queue.pause_reason = `Next up paused when you stopped “${task.title}”, so nothing starts by itself. Resume when you’re ready.`;
    this.talk.invalidate(queue); this.talk.put(queue);
    const count = (this.#stopCounts.get(run.run_id) ?? 0) + (force ? 0 : 1);
    this.#stopCounts.set(run.run_id, count);
    this.#withdrawHeldMessages(taskId, run.run_id);
    const unacknowledgedExit = !force && this.stopSafety.endsProcess(taskId);
    if (!force && !unacknowledgedExit && this.stopSafety.handle(task, run, count)) return;
    if (stillFinishing(run)) return this.#stopLeftovers(task, run, count, force);
    this.#touchRun(run.run_id, { state: "stopping" });
    this.#touchTask(taskId, { status_line: "Stop requested", wake: undefined });
    const facts: ReceiptFact[] = [
      { label: "Agent confirmed Stop", status: force || unacknowledgedExit ? "unknown" : "pending", detail: null },
      { label: "Stop requests", status: "done", detail: `Stop sent ${stopFrequency(count)}${force ? " · then Force stop" : ""}` },
      ...(force ? [{ label: "Agent process ended on the computer", status: "done" as const, detail: "Files already changed stay changed; interrupted writes may be incomplete." }] : []),
    ];

    this.#later(unacknowledgedExit ? 5_500 : 900, () => {
      const final: ReceiptFact[] = facts.map((f) =>
        f.status === "pending" ? { ...f, status: "done" } : f,
      );
      if (unacknowledgedExit) final.push({ label: "Agent process ended on the computer", status: "done",
        detail: "The agent didn’t answer Stop within 5 s, so the computer ended its process. Files already changed stay changed; interrupted writes may be incomplete. Detached or remote children are reported separately." });
      final.push(...this.children.stopChildren(run));
      final.push({ label: "Shell commands started by the agent", status: "unknown", detail: "Commands the agent ran outside its helpers aren’t tracked." });
      final.push({ label: "Desktop access withdrawn from the agent", status: "done", detail: null });
      final.push({ label: "Next up paused", status: "done", detail: queue.pause_reason ?? null });
      this.#touchRun(run.run_id, { state: "interrupted", ended_at: Date.now(), holds_writer: false });
      this.#failQueuedMessages(taskId, run.run_id);
      this.#append(taskId, run.run_id, { kind: "run_ended", run_id: run.run_id, state: "interrupted" });
      const receipt = this.#intervention(taskId, task.target_id, "stop_task", final);
      this.#append(taskId, run.run_id, { kind: "receipt", receipt });
      this.#touchTask(taskId, { state: this.#ended(taskId, "interrupted"), status_line: null });
      this.#releaseWriter(task.project_id, run.run_id);
      if (task.target_id) {
        const v = this.#control(task.target_id);
        this.#setControl({ ...v, input_state: "ready", lease: null, model_observation: null });
      }
      for (const ap of this.approvals.filter((a) => a.run_id === run.run_id && a.state === "pending")) {
        this.#upsertApproval({ ...ap, state: "cancelled", revision: ap.revision + 1, resolved_at: Date.now() });
      }
    });
  }
  /** Stop after the turn ended reaches only what it left running. The turn's
   * outcome stands; the run lets go of the project once that has ended. */
  #stopLeftovers(task: Task, run: Run, count: number, force: boolean): void {
    this.#later(900, () => {
      const helpers = this.children.stopChildren(this.#run(run.run_id));
      // A helper still running keeps the agent's process, and the run, alive.
      const ended = force || helpers.every(f => f.status === "done");
      const facts: ReceiptFact[] = [
        { label: "Agent confirmed Stop", status: "not_applicable", detail: "Its turn had already finished." },
        { label: "Stop requests", status: "done", detail: `Stop sent ${stopFrequency(count)}${force ? " · then Force stop" : ""}` },
        { label: "Agent process ended on the computer", status: ended ? "done" : "unknown", detail: force
          ? "Files already changed stay changed; interrupted writes may be incomplete. Detached or remote children are reported separately."
          : ended ? "It ended once its helpers had. Files already changed stay changed."
            : "It, or something it started, is still running on the computer. Stop again, or force stop it." },
        ...helpers,
        { label: "Desktop access withdrawn from the agent", status: "done", detail: null },
        { label: "Next up paused", status: "done", detail: this.talk.queue(task.project_id).pause_reason ?? null },
      ];
      if (ended) {
        this.#touchRun(run.run_id, { cleanup_pending: false, holds_writer: false });
        this.#releaseWriter(task.project_id, run.run_id);
      }
      this.#append(task.task_id, run.run_id, { kind: "receipt", receipt: this.#intervention(task.task_id, task.target_id, "stop_task", facts) });
    });
  }



  #intervention(taskId: string | null, targetId: string | null, kind: InterventionReceipt["kind"], facts: ReceiptFact[]): InterventionReceipt {
    const receipt: InterventionReceipt = { kind, requested_at: Date.now() - 900, facts, summary: summarize(kind, facts) };
    if (kind === "stop_task" && facts.some(f => ["still_active", "unknown", "pending"].includes(f.status))) receipt.summary = "Stop returned; some work remains unconfirmed";
    if (kind === "stop_task" && facts.some(f => f.label === "Agent process ended on the computer" && f.status === "done" && f.detail?.includes("within 5 s"))) receipt.summary = "Agent process ended on the computer. Prior file changes remain.";
    this.emit({ type: "intervention_recorded", task_id: taskId, target_id: targetId, receipt });
    return receipt;
  }

  #upsertApproval(a: Approval): void {
    this.approvals = this.approvals.map((x) => (x.approval_id === a.approval_id ? a : x));
    this.emit({ type: "approval_upserted", approval: a });
    if (a.state !== "pending") {
      for (const at of this.attention.filter((x) => x.approval_id === a.approval_id && !x.resolved_at)) {
        at.resolved_at = Date.now();
        this.emit({ type: "attention_resolved", attention_id: at.attention_id });
      }
      this.#recountAttention(a.task_id);
    }
  }

  /** An agent's run asks the owner: the request, its attention item and its line in the transcript. */
  ask(a: Approval, item: AttentionItem): void {
    this.approvals = [...this.approvals, a];
    this.emit({ type: "approval_upserted", approval: a });
    this.attention.push(item);
    this.emit({ type: "attention_upserted", item });
    this.emit({ type: "timeline_appended", task_id: a.task_id, item: { seq: this.seq + 1, at: a.requested_at, run_id: a.run_id, item: { kind: "approval", approval_id: a.approval_id } } });
    this.#recountAttention(a.task_id);
  }

  /** Settles a request the computer answered itself, as a filled code does. */
  settleApproval(id: string, state: "allowed" | "denied"): void {
    const a = this.approvals.find((x) => x.approval_id === id);
    if (!a) throw err("not_found", "That request no longer exists");
    this.#upsertApproval({ ...a, state, revision: a.revision + 1, resolved_by: { kind: "device", device_id: fx.ids.device }, resolved_at: Date.now() });
    this.#touchTask(a.task_id, { state: "working" });
  }

  #resolveApproval(id: string, decision: Extract<Command, { command: "resolve_approval" }>["decision"], expectedRevision: number | null): void {
    const a = this.approvals.find((x) => x.approval_id === id);
    if (!a) throw err("not_found", "That request no longer exists");
    if (a.state === "cancelled") throw err("expired", "Arrived after the run ended; not applied");
    if (a.state !== "pending") throw { ...err("already_resolved", `Already ${a.state.replace("_", " ")}`), detail: { state: a.state } };
    if (expectedRevision !== a.revision)
      throw { ...err("stale_revision", "The request changed. Review it again."), detail: { revision: a.revision } };
    // A code goes into the code prompt, which fills it.
    if (a.kind === "login_code" && decision.decision !== "deny") throw err("invalid_request", "Type the code in the code prompt.");
    if (this.failNextApprovalDelivery) {
      this.failNextApprovalDelivery = false;
      const error = err("native_failure", "The agent did not receive this. It may still be waiting.");
      this.#upsertApproval({ ...a, revision: a.revision + 1, delivery: { decision, at: Date.now(), error } });
      throw error;
    }
    const allowed = decision.decision !== "deny";
    this.#upsertApproval({
      ...a,
      state: allowed ? "allowed" : "denied",
      delivery: { decision, at: Date.now(), error: null },
      revision: a.revision + 1,
      resolved_by: { kind: "device", device_id: fx.ids.device },
      resolved_at: Date.now(),
    });
    if (a.kind === "login_use" || a.kind === "login_code") this.agentLogins.decided(a, allowed);
    const task = this.#task(a.task_id);
    this.#touchRun(a.run_id, { state: "working" });
    this.#touchTask(a.task_id, { state: "working", status_line: allowed ? "Running tests" : "Continuing without that" });
    if (allowed && a.approval_id === fx.ids.aTest) {
      this.#later(400, () => {
        this.#append(a.task_id, a.run_id, {
          kind: "activity",
          tool: "Bash",
          summary: "cargo test -p payments-client -- --test-threads=1",
          status: "succeeded",
          detail: "test result: ok. 64 passed; 0 failed",
        });
        this.#later(200, () =>
          this.#script(task.task_id, a.run_id, {
            activity: [{ tool: "Bash", summary: "Ran retries_on_503 200 times under load", detail: "200/200 passed" }],
            text:
              "All 64 payments tests pass serially, and `retries_on_503` passed 200 times in a row under load (it failed 41 of 200 before).\n\nThe fix injects `Clock` into `RetryPolicy`; production still uses `SystemClock`, so behavior outside tests is unchanged.",
            summary: "Fixed the flaky retry test by injecting the clock; 200/200 stress runs pass.",
            files: [
              { path: "crates/payments-client/src/retry.rs", change: "modified", added: 23, removed: 11 },
              { path: "crates/payments-client/tests/retry.rs", change: "modified", added: 18, removed: 6 },
            ],
            checks: [
              { name: "Payments tests (serial)", command: "cargo test -p payments-client -- --test-threads=1", status: "passed", detail: "64 passed", provenance: "native" },
              { name: "Stress: retries_on_503 ×200", command: null, status: "passed", detail: "200/200", provenance: "native" },
              { name: "Full workspace tests", command: "cargo test --workspace", status: "not_run", detail: null, provenance: "inferred" },
            ],
            unknowns: ["Whether other tests use the real clock the same way"],
          }),
        );
      });
    } else if (!allowed) {
      this.#later(300, () =>
        this.#script(
          task.task_id,
          a.run_id,
          {
            activity: [],
            text: "Understood — I won't run that. The change is in place but untested. Tell me how you'd like me to verify it.",
            summary: "The change is in place but untested; the requested test was denied.",
          },
        ),
      );
    }
  }

  // ---- Queue ---------------------------------------------------------------

  #queueEdit(c: Extract<Command, { command: "queue_move" | "queue_remove" | "queue_set_authorization" | "queue_set_dependencies" }>): void {
    let items = [...this.queue.items];
    const idx = items.findIndex((i) => i.item_id === c.item_id);
    if (idx < 0) throw err("not_found", "That item already left Next up");
    const item = items[idx]!;
    switch (c.command) {
      case "queue_move":
        items.splice(idx, 1);
        items.splice(Math.max(0, Math.min(c.position, items.length)), 0, item);
        break;
      case "queue_remove":
        items.splice(idx, 1);
        this.#touchTask(item.task_id, { state: "archived", status_line: "Removed from Next up" });
        break;
      case "queue_set_authorization":
        items[idx] = { ...item, authorization: c.authorization };
        break;
      case "queue_set_dependencies":
        items[idx] = { ...item, depends_on: c.depends_on };
        break;
    }
    items = items.map((it, i) => ({ ...it, position: i }));
    this.queue = { ...this.queue, items, revision: this.queue.revision + 1 };
    this.emit({ type: "queue_changed", queue: this.queue });
  }

  // ---- Control -------------------------------------------------------------

  #control(targetId: string): TargetControlView {
    const v = this.control.find((x) => x.target_id === targetId);
    if (!v) throw err("not_found", "That display is no longer available");
    return v;
  }

  #setControl(v: TargetControlView): void {
    this.control = this.control.map((x) => (x.target_id === v.target_id ? v : x));
    this.emit({ type: "control_changed", view: v }, { kind: "platform" });
  }

  #target(targetId: string): ControlTarget {
    const t = this.targets.find((x) => x.target_id === targetId);
    if (!t) throw err("not_found", "That display is no longer available");
    return t;
  }

  #grantHuman(targetId: string, geometryRevision: number) {
    this.controlSim.admitInput(targetId);
    const target = this.#target(targetId);
    if (target.input.state !== "available")
      throw err("unsupported", target.input.state === "unsupported" ? target.input.reason : "Input is not available here");
    if (target.geometry.revision !== geometryRevision) throw err("stale_revision", "The display changed. Look again before taking control.");
    const v = this.#control(targetId);
    if (v.input_state === "unknown")
      throw err("outcome_unknown", "Input state is unknown; retry Pause agent to establish the native barrier before requesting control");
    if (v.input_state === "agent" || v.input_state === "revoking")
      throw err("conflict", "The agent has input. Use Take over to stop its input first.");
    const lease = {
      lease_id: uuidv7(Date.now()),
      target_id: targetId,
      owner: { kind: "human" as const, device_id: fx.ids.device },
      generation: ++this.#leaseGen,
      boot_epoch: this.bootEpoch,
      state: "active" as const,
      granted_at: Date.now(),
      renewal_window_ms: 5000,
    };
    this.#setControl({ ...v, input_state: "human", lease });
    return lease;
  }

  #releaseLease(leaseId: string): void {
    const v = this.control.find((x) => x.lease?.lease_id === leaseId);
    if (!v?.lease) throw err("not_found", "That control session already ended");
    // As on the computer: input stays paused under the ended lease until
    // someone gives it back or takes over.
    this.#setControl({ ...v, input_state: "paused", lease: { ...v.lease, state: "revoked" } });
    this.#intervention(null, v.target_id, "leave_control", [
      { label: "Held keys and buttons released", status: "done", detail: null },
      { label: "Your control ended", status: "done", detail: null },
    ]);
  }

  #pauseInput(targetId: string): void {
    const v = this.#control(targetId);
    if (v.input_state === "unknown") {
      // The native barrier: nothing queued before this can still arrive.
      this.#setControl({ ...v, input_state: "paused", lease: null });
      return;
    }
    if (v.input_state !== "agent") throw err("conflict", "The agent does not have input on this display");
    this.#setControl({ ...v, input_state: "revoking", lease: v.lease ? { ...v.lease, state: "revoking" } : null });
    const runId = v.lease?.owner.kind === "agent" ? v.lease.owner.run_id : null;
    const task = runId ? this.tasks.find((t) => t.current_run === runId) : undefined;
    this.#later(600, () => {
      const cur = this.#control(targetId);
      if (cur.lease?.lease_id !== v.lease?.lease_id || (runId && this.runs.find((r) => r.run_id === runId)?.state === "stopping")) return;
      this.#setControl({ ...cur, input_state: "paused", lease: cur.lease ? { ...cur.lease, state: "revoked" } : null });
      const receipt = this.#intervention(task?.task_id ?? null, targetId, "pause_input", [
        { label: "Agent input revoked", status: "done", detail: "Input release confirmed by the computer" },
        { label: "Late agent actions rejected", status: "done", detail: null },
        { label: "Agent keeps reasoning and may read files", status: "still_active", detail: "Pause agent does not stop the task" },
      ]);
      if (task) {
        this.#append(task.task_id, runId, { kind: "receipt", receipt });
        this.#touchTask(task.task_id, { state: "awaiting_decision", status_line: "Agent paused — resume it when you’re ready" });
      }
    });
  }

  async #takeOver(targetId: string, geometryRevision: number) {
    const v = this.#control(targetId);
    const runId = v.lease?.owner.kind === "agent" ? v.lease.owner.run_id : null;
    if (v.input_state === "agent" || v.input_state === "paused") {
      this.#setControl({ ...v, input_state: "revoking", lease: null });
      await sleep(500 * this.#pace);
      this.#setControl({ ...this.#control(targetId), input_state: "ready", lease: null });
    }
    const lease = this.#grantHuman(targetId, geometryRevision);
    const task = runId ? this.tasks.find((t) => t.current_run === runId) : undefined;
    const receipt = this.#intervention(task?.task_id ?? null, targetId, "take_over", [
      { label: "Agent input revoked", status: runId ? "done" : "not_applicable", detail: null },
      { label: "Held agent keys released", status: runId ? "done" : "not_applicable", detail: null },
      { label: "This device now has input", status: "done", detail: "New input permission confirmed" },
    ]);
    if (task) {
      this.#append(task.task_id, runId, { kind: "receipt", receipt });
      this.#touchTask(task.task_id, { state: "awaiting_decision", status_line: "You took over" });
    }
    return lease;
  }

  #returnToAgent(targetId: string, runId: string): void {
    const v = this.#control(targetId);
    const run = this.#run(runId);
    if (isTerminalRun(run.state) || run.state === "stopping" || v.private_step) throw err("conflict", "That run cannot resume input in its current state");
    this.#setControl({
      ...v,
      input_state: "agent",
      lease: {
        lease_id: uuidv7(Date.now()),
        target_id: targetId,
        owner: { kind: "agent", run_id: runId },
        generation: ++this.#leaseGen,
        boot_epoch: this.bootEpoch,
        state: "active",
        granted_at: Date.now(),
        renewal_window_ms: 5000,
      },
      model_observation: v.model_observation
        ? { ...v.model_observation, state: "allowed" }
        : { observation_id: uuidv7(Date.now()), run_id: runId, target_id: targetId, state: "allowed", delivered_count: 0 },
    });
    this.#touchTask(run.task_id, { state: "working", status_line: "Looking at the screen again" });
  }

  #privateStep(targetId: string, start: boolean): void {
    this.computer.privateStep(targetId, start);
  }

  handleInput(env: InputEnvelope): InputAck {
    const refusal = this.controlSim.refusal(env);
    if (refusal) return refusal;
    const v = this.control.find((x) => x.lease?.lease_id === env.lease_id);
    if (!v || !v.lease || v.lease.owner.kind !== "human") return { lease_id: env.lease_id, seq: env.seq, outcome: "stale_lease" };
    if (v.lease.state !== "active" || v.input_state !== "human" || v.lease.generation !== env.generation) return { lease_id: env.lease_id, seq: env.seq, outcome: "stale_lease" };
    const t = this.targets.find((x) => x.target_id === v.target_id);
    if (t && t.geometry.revision !== env.geometry_revision) return { lease_id: env.lease_id, seq: env.seq, outcome: "stale_geometry" };
    const last = this.controlSim.inputs.filter((input) => input.lease_id === env.lease_id).at(-1);
    if (last && last.seq >= env.seq) return { lease_id: env.lease_id, seq: env.seq, outcome: "out_of_order" };
    this.controlSim.inputs.push(env);
    if (this.controlSim.uncertainNext) {
      // As on the computer: the lease is revoked, and without an acknowledged
      // barrier the input state stays unknown until Pause agent succeeds.
      this.controlSim.uncertainNext = false;
      this.#setControl({ ...v, input_state: "unknown", lease: { ...v.lease, state: "revoked" } });
      return { lease_id: env.lease_id, seq: env.seq, outcome: "unknown" };
    }
    this.lastInput = env;
    return { lease_id: env.lease_id, seq: env.seq, outcome: "injected" };
  }

  lastInput: InputEnvelope | null = null;

  // ---- Background activity -----------------------------------------------

  #startBackground(): void {
    const steps = [
      { tool: "computer.observe", summary: "Looked at DP-1", detail: null },
      { tool: "computer.pointer", summary: "Scrolled the dashboard to “Latency (1h)”", detail: null },
      { tool: "computer.observe", summary: "Read the chart legend", detail: "p50, p95" },
      { tool: "computer.pointer", summary: "Hovered the p95 line at 14:20", detail: null },
    ];
    let i = 0;
    const tick = () => {
      const run = this.runs.find((r) => r.run_id === fx.ids.rDash);
      const v = this.control.find((x) => x.target_id === fx.ids.tgDp1);
      if (run && run.state === "working" && v?.input_state === "agent") {
        const s = steps[i % steps.length]!;
        i++;
        this.live({ kind: "activity_status", task_id: fx.ids.tDash, run_id: fx.ids.rDash, text: s.summary });
        this.#append(fx.ids.tDash, fx.ids.rDash, { kind: "activity", tool: s.tool, summary: s.summary, status: "succeeded", detail: s.detail });
        if (v.model_observation) this.#setControl({ ...v, model_observation: { ...v.model_observation, delivered_count: v.model_observation.delivered_count + 1 } });
        this.#touchTask(fx.ids.tDash, { status_line: s.summary });
      }
      this.#later(7000, tick);
    };
    this.#later(5000, tick);
  }

  #setTransfer(next: TransferRecord): void {
    this.transfers = this.transfers.map((t) => (t.transfer_id === next.transfer_id ? next : t));
    this.emit({ type: "transfer_upserted", transfer: next });
  }

  /** Bytes moved on the transfer link (SimTransferLink), in either direction. */
  transferProgress(id: string, transferred: number): TransferRecord | null {
    const tr = this.transfers.find((t) => t.transfer_id === id);
    if (!tr || tr.state.state === "cancelled" || tr.state.state === "completed") return null;
    const next: TransferRecord = { ...tr, transferred, state: { state: "in_progress" }, updated_at: Date.now() };
    this.#setTransfer(next);
    return next;
  }

  /** Keep bytes as an artifact the phone can download (host captures, clips). */
  addArtifact(bytes: Uint8Array, mediaType: string, sha: string, label: string | null, taskId: string | null = null, capture?: ArtifactMeta["capture"], links?: ArtifactMeta["links"]): ArtifactMeta {
    const artifact: ArtifactMeta = {
      artifact_id: uuidv7(Date.now()),
      media_type: mediaType,
      size: bytes.length,
      sha256: sha,
      created_at: Date.now(),
      label,
      task_id: taskId,
      capture,
      links,
    };
    this.artifactBytes.set(artifact.artifact_id, bytes);
    this.emit({ type: "artifact_created", artifact });
    return artifact;
  }

  /** The bytes a declared download will send, or null once cancelled. */
  downloadBytes(id: string): { bytes: Uint8Array; sha256: string } | null {
    return this.#downloads.get(id) ?? null;
  }

  finishDownload(id: string): void {
    const tr = this.transfers.find((t) => t.transfer_id === id);
    this.#downloads.delete(id);
    if (tr) this.#setTransfer({ ...tr, transferred: tr.size, state: { state: "completed" }, updated_at: Date.now() });
  }

  async #downloadSource(src: DownloadSource): Promise<{ name: string; bytes: Uint8Array; hostPath: string | null }> {
    switch (src.kind) {
      case "artifact": {
        const bytes = this.artifactBytes.get(src.artifact_id);
        if (!bytes) throw err("not_found", "That file is no longer on the computer");
        // Named as the companion names it: its label, else "Attachment".
        const label = this.work.artifacts().find((a) => a.artifact_id === src.artifact_id)?.label;
        return { name: label ?? "Attachment", bytes, hostPath: null };
      }
      case "library_file": {
        const file = await this.#read(src.root_id, src.path);
        if (!file) throw err("not_found", "That file is no longer on the computer");
        const root = this.roots.find((r) => r.root_id === src.root_id);
        return {
          name: src.path.split("/").pop() || src.path,
          bytes: new TextEncoder().encode(file.text),
          hostPath: root ? `${root.host_path}/${src.path}` : null,
        };
      }
      case "project_file":
        throw err("unsupported", "Project files aren’t available in the simulated computer");
      case "app_update":
        if (src.sha256 !== SIM_APP_SHA) throw err("not_found", "That app update isn’t available.");
        return { name: "ophio-sim.apk", bytes: SIM_APP_BYTES, hostPath: null };
      case "checklist_file":
        return { ...(await this.checklists.file(src.project_id, src.sha256)), hostPath: null };
    }
  }

  /** All bytes received: verify and place at the declared destination. */
  async finishUpload(
    id: string,
    sha256: string,
    mediaType: string,
    bytes?: Uint8Array,
  ): Promise<{ ok: true; artifactId: string | null } | { ok: false; reason: string }> {
    const tr = this.transfers.find((t) => t.transfer_id === id);
    if (!tr) return { ok: false, reason: "No such transfer" };
    const fail = (reason: string) => {
      const next: TransferRecord = { ...tr, state: { state: "failed", reason }, updated_at: Date.now() };
      this.transfers = this.transfers.map((t) => (t.transfer_id === id ? next : t));
      this.emit({ type: "transfer_upserted", transfer: next });
      return { ok: false as const, reason };
    };
    if (tr.state.state === "cancelled") return { ok: false, reason: "Transfer cancelled" };
    if (tr.state.state === "completed") return { ok: false, reason: "Transfer already completed" };
    if (tr.transferred !== tr.size) return fail(`Received ${tr.transferred} of ${tr.size} bytes`);
    if (tr.sha256 && tr.sha256 !== sha256) return fail("The file changed while sending");
    let artifactId: string | null = null;
    let hostPath: string | null = null;
    const dest = tr.destination ?? { kind: "inbox" };
    if (dest.kind === "artifact") {
      const evidence = this.uploadEvidence.get(id);
      const taskId = evidence?.task_id ?? this.uploadTasks.get(id) ?? null;
      const artifact = this.addArtifact(bytes ?? new Uint8Array(), mediaType, sha256, tr.name, taskId, undefined, evidence ? [evidence] : undefined);
      if (taskId && !evidence) this.emit({ type: "artifact_created", artifact: { ...artifact, links: [{ task_id: taskId, run_id: null, annotation: null, role: "source" }] } });
      artifactId = artifact.artifact_id;
    } else if (dest.kind === "project_dir") {
      const project = this.projects.find((p) => p.project_id === dest.project_id);
      hostPath = `${project?.host_path ?? "/home/sam"}/${dest.dir ? `${dest.dir}/` : ""}${tr.name}`;
    } else {
      hostPath = `${tr.destination_folder ?? "/home/sam/Inbox"}/${tr.name}`;
    }
    if (hostPath) {
      const occupied = new Set(this.transfers.flatMap(t => t.host_path ? [t.host_path] : []));
      for (const root of this.roots) for (const path of Object.keys(this.files[root.root_id] ?? {})) occupied.add(`${root.host_path}/${path}`);
      const decision = this.libraryWorkflows.collisions.get(id);
      if (occupied.has(hostPath) && decision?.choice !== "replace") return fail("Destination exists. Review a collision choice.");
      if (decision?.choice === "replace" && dest.kind === "project_dir") {
        const root = this.roots.find(r => r.project_id === dest.project_id && r.writable);
        const path = dest.dir ? `${dest.dir}/${tr.name}` : tr.name;
        const old = root ? this.files[root.root_id]?.[path] : undefined;
        if (!root || old === undefined || await libraryHash(old) !== decision.revision) return fail("Destination changed. Review it again.");
        await this.libraryWorkflows.deleted(root.root_id, path, old);
        const files = this.files[root.root_id];
        if (files) files[path] = new TextDecoder().decode(bytes);
      }

    }
    if (hostPath && this.interruptPublication) {
      // As on the computer: the destination was recorded before the final
      // move, and the move's outcome is unconfirmed. Nothing may resume it.
      this.interruptPublication = false;
      const root = dest.kind === "project_dir" ? (this.projects.find((p) => p.project_id === dest.project_id)?.host_path ?? "/home/sam") : (tr.destination_folder ?? "/home/sam/Inbox");
      const next: TransferRecord = {
        ...tr,
        sha256,
        state: { state: "reconciliation_required", reason: "Publication is unconfirmed; inspect the recorded destination, then cancel to clear" },
        publication: { root, path: hostPath.slice(root.length + 1) },
        updated_at: Date.now(),
      };
      this.#setTransfer(next);
      return { ok: false, reason: "The computer couldn’t confirm the file was put in place" };
    }
    this.uploadStaging.remove(id);
    const next: TransferRecord = { ...tr, sha256, state: { state: "completed" }, host_path: hostPath, updated_at: Date.now() };
    this.transfers = this.transfers.map((t) => (t.transfer_id === id ? next : t));
    this.emit({ type: "transfer_upserted", transfer: next });
    return { ok: true, artifactId };
  }

  #readiness(at: number): ReadinessReport {
    return {
      checked_at: at,
      checks: [
        { area: "network", label: "Private network (Tailscale)", status: "passed", detail: "studio.tail1234.ts.net", remedy: null },
        ...variantChecks(this.variant, this.indicator),
        { area: "terminal", label: "Terminal sessions", status: "passed", detail: null, remedy: null },
        { area: "accounts", label: "Claude Code sign-in", status: "passed", detail: "Personal · Max", remedy: null },
        { area: "accounts", label: "Claude Code steer running", status: "unknown", remedy: null,
          detail: "Messages can be delivered, but Claude Code does not confirm whether they change the current turn.",
          summary: "Not known whether you can steer Claude Code while it works" },
        { area: "accounts", label: "Codex sign-in", status: "passed", detail: "Personal · Pro", remedy: null },
        { area: "accounts", label: "Codex computer use", status: "passed", detail: null, remedy: null,
          summary: "Codex can use the computer for you" },
        { area: "accounts", label: "Pi computer use", status: "not_run", remedy: null,
          detail: `Pi runs only in a terminal, so ${PRODUCT_NAME} can’t give it the computer tools`,
          summary: "Pi can’t use the computer for you" },
        { area: "speech", label: "Speech model", status: "passed", detail: "whisper small.en", remedy: null },
        channelCheck("Telegram", this.host.capabilities.telegram),
        channelCheck("Discord", this.host.capabilities.discord),
      ],
    };
  }
}

// ---- Scripted replies -------------------------------------------------------

interface ScriptedReply {
  activity: { tool: string; summary: string; detail?: string }[];
  text: string;
  summary?: string;
  files?: NonNullable<Run["outcome"]>["changed_files"];
  checks?: NonNullable<Run["outcome"]>["checks"];
  unknowns?: string[];
}

function replyFor(prompt: string): ScriptedReply {
  const p = prompt.toLowerCase();
  if (p.includes("readme")) {
    return {
      activity: [
        { tool: "Read", summary: "Read README.md" },
        { tool: "Read", summary: "Read cli/Cargo.toml" },
        { tool: "Edit", summary: "Edited README.md", detail: "+9 −4" },
      ],
      text: "Updated the install section: it now uses `cargo install tidewater-cli` and mentions the `--locked` flag. I kept the old path-based instructions under “From source”.",
      files: [{ path: "README.md", change: "modified", added: 9, removed: 4 }],
      checks: [{ name: "Markdown links", command: null, status: "passed", detail: "12 links checked", provenance: "observed" }],
    };
  }
  if (p.includes("?") && p.length < 160) {
    return {
      activity: [{ tool: "Grep", summary: "Searched the project" }],
      text: "Short answer: yes. The retry policy only retries 502, 503 and 504, with exponential backoff starting at 50 ms and at most 3 attempts. It's configured in `src/limiter/mod.rs` and documented in `docs/retry-policy.md`.",
    };
  }
  return {
    activity: [
      { tool: "Read", summary: "Read the relevant files" },
      { tool: "Edit", summary: "Made the change", detail: "+14 −3" },
      { tool: "Bash", summary: "cargo test --workspace", detail: "212 passed" },
    ],
    text: "Done. I made the change and the full test suite passes. I didn't touch anything outside the files listed below.",
    files: [{ path: "src/http/orders.rs", change: "modified", added: 14, removed: 3 }],
    checks: [{ name: "cargo test", command: "cargo test --workspace", status: "passed", detail: "212 passed", provenance: "native" }],
  };
}

/** Ask and Plan runs read and answer; they never report edits or commands. */
function readOnlyReply(mode: TaskMode): ScriptedReply {
  return mode === "plan"
    ? {
        activity: [
          { tool: "Glob", summary: "Listed src/**/*.rs" },
          { tool: "Read", summary: "Read src/http/orders.rs" },
          { tool: "Grep", summary: "Searched for retry_policy" },
        ],
        text: "Plan, nothing changed yet:\n\n1. Move the retry limits in `src/http/orders.rs` into `RetryPolicy`.\n2. Add a test for the 504 path in `tests/orders.rs`.\n3. Update `docs/retry-policy.md`.\n\nStart a Work task with this plan when you're ready.",
        summary: "Proposed a 3-step plan; nothing changed",
      }
    : {
        activity: [
          { tool: "Grep", summary: "Searched the project" },
          { tool: "Read", summary: "Read docs/retry-policy.md" },
        ],
        text: "Retries cover 502, 503 and 504 only, with exponential backoff from 50 ms and at most 3 attempts. The limits live in `src/limiter/mod.rs`. I only read files; nothing was changed.",
      };
}

/** What a Summarize turn answers in the simulator. */
function summaryReply(): ScriptedReply {
  return {
    activity: [{ tool: "Read", summary: "Read src/http/orders.rs" }],
    text: "Here’s what happened:\n\n- You asked me to make order retries respect `RetryPolicy`.\n- I moved the retry limits into `src/http/orders.rs` and added a 504 test in `tests/orders.rs`.\n- `cargo test` passed (42 tests). I didn’t update `docs/retry-policy.md` yet.\n\nNext I’d update that doc. I only read files for this answer; nothing was changed.",
    summary: "Summarized the task; nothing changed",
  };
}

function steerReply(text: string): ScriptedReply {
  return {
    activity: [],
    text: !text.trim()
      ? "Got it — I'll work from your markup. Continuing with that in mind."
      : `Got it — ${text.length > 80 ? "I'll adjust course" : `“${text.trim()}”`}. Continuing with that in mind.`,
  };
}

/** A chat connector's readiness, worded as the computer words it. */
function channelCheck(label: string, a: Availability): ReadinessCheck {
  return {
    area: "channels",
    label,
    status: a.state === "available" ? "passed" : a.state === "unknown" ? "unknown"
      : a.state === "permission_missing" || a.state === "unsupported" ? "not_run" : "failed",
    detail: a.state === "permission_missing" ? "Needs setup." : "reason" in a ? a.reason : null,
    remedy: a.state === "permission_missing" ? a.remedy : a.state === "revoked" ? "Enable the feature again on the computer" : null,
  };
}

function titleFrom(prompt: string): string {
  const first = prompt.trim().split(/\n|(?<=[.!?])\s/)[0] ?? prompt;
  return first.length > 72 ? `${first.slice(0, 69).trimEnd()}…` : first;
}

function harnessLabel(kind: string): string {
  return fx.harnesses.find((h) => h.kind === kind)?.display_name ?? kind;
}

function summarize(kind: InterventionReceipt["kind"], facts: ReceiptFact[]): string {
  const unknown = facts.filter((f) => f.status === "unknown" || f.status === "still_active").length;
  const lead: Record<InterventionReceipt["kind"], string> = {
    stop_task: "Task stopped",
    pause_input: "Agent input paused",
    take_over: "You have control",
    leave_control: "You left control",
    private_step: "Private step",
    pause_queue: "Next up paused",
    lock_desktop: "Lock requested",
    window_action: "Window action requested",
  };
  return unknown ? `${lead[kind]}. ${unknown} ${unknown === 1 ? "item" : "items"} not confirmed.` : `${lead[kind]}.`;
}

export function isTerminalRun(s: RunState): boolean {
  return s === "completed" || s === "failed" || s === "interrupted" || s === "reconciliation_required";
}

export function isTerminalTask(s: TaskState): boolean {
  return s === "completed" || s === "failed" || s === "interrupted" || s === "archived" || s === "reconciliation_required";
}

function isProtocolError(e: unknown): e is ProtocolError {
  return typeof e === "object" && e !== null && "code" in e && "message" in e;
}


export const allGrants = {
  view: true,
  control_input: true,
  terminal: true,
  tasks: true,
  approvals: true,
  computer_use: true,
  library_read: true,
  library_write: true,
  transfers: true,
  admin: true,
  accounts: true,
};
