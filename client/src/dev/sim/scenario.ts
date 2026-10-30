// SPDX-License-Identifier: Apache-2.0
import toolApprovalFixture from "../../../../crates/ports/test-fixtures/tool-approvals.json";
import { blockingAttentionCount } from "./attention";
import { commits as reviewCommits, prReviews, signingApproval, stamped } from "./reviews";
import { PROGRAM_NAME } from "../../brand";
//
// Development-only fixture data for the simulated companion. Never bundled
// into release builds (see src/host/index.ts).

import type {
  AgentFileGroup,
  Approval,
  BranchListing,
  AttentionItem,
  ControlTarget,
  HarnessInfo,
  HostCapabilities,
  HostInfo,
  LibraryRoot,
  PreviewEndpoint,
  Project,
  QueueView,
  Run,
  TargetControlView,
  Task,
  TerminalInfo,
  TimelineBody,
} from "../../protocol/types";

const MIN = 60_000;
const available = { state: "available" } as const;

/** One tool request and the approval the companion summarizes it as. */
export interface ApprovalCase {
  tool: string;
  input: unknown;
  approval: Pick<Approval, "title" | "detail" | "detail_lines" | "questions" | "scope" | "shortened"> & { kind: string };
}

/** The companion's approval shapes, for shot plans and tests that stage one. */
export const approvalCases = toolApprovalFixture as ApprovalCase[];
const toolApprovalCases = approvalCases;

export const ids = {
  host: "0192f7a1-0000-7000-8000-00000000a001",
  boot: "0192f7a1-0000-7000-8000-00000000b001",
  device: "0192f7a1-0000-7000-8000-00000000d001",
  pHome: "0192f7a1-0000-7000-8000-000000000100",
  pTide: "0192f7a1-0000-7000-8000-000000000101",
  pNotes: "0192f7a1-0000-7000-8000-000000000102",
  pGarden: "0192f7a1-0000-7000-8000-000000000103",
  pZine: "0192f7a1-0000-7000-8000-000000000104",
  tFlaky: "0192f7a1-0000-7000-8000-000000000201",
  tHeaders: "0192f7a1-0000-7000-8000-000000000202",
  tReadme: "0192f7a1-0000-7000-8000-000000000203",
  tDash: "0192f7a1-0000-7000-8000-000000000204",
  tNotes: "0192f7a1-0000-7000-8000-000000000205",
  tSigning: "0192f7a1-0000-7000-8000-000000000206",
  tExport: "0192f7a1-0000-7000-8000-000000000207",
  rFlaky: "0192f7a1-0000-7000-8000-000000000301",
  rHeaders: "0192f7a1-0000-7000-8000-000000000302",
  rDash: "0192f7a1-0000-7000-8000-000000000304",
  aTest: "0192f7a1-0000-7000-8000-000000000401",
  atApproval: "0192f7a1-0000-7000-8000-000000000501",
  atReady: "0192f7a1-0000-7000-8000-000000000502",
  tgDp1: "0192f7a1-0000-7000-8000-000000000601",
  tgHdmi: "0192f7a1-0000-7000-8000-000000000602",
  tgWin: "0192f7a1-0000-7000-8000-000000000603",
  lease1: "0192f7a1-0000-7000-8000-000000000701",
  obs1: "0192f7a1-0000-7000-8000-000000000801",
  rootSkills: "0192f7a1-0000-7000-8000-000000000901",
  rootInstr: "0192f7a1-0000-7000-8000-000000000902",
  rootNotes: "0192f7a1-0000-7000-8000-000000000903",
  rootAgents: "0192f7a1-0000-7000-8000-000000000904",
  rootOphio: "0192f7a1-0000-7000-8000-000000000905",
  rootClaude: "0192f7a1-0000-7000-8000-000000000906",
  term1: "0192f7a1-0000-7000-8000-000000000a01",
  term2: "0192f7a1-0000-7000-8000-000000000a02",
  qReadme: "0192f7a1-0000-7000-8000-000000000b01",
  preview1: "0192f7a1-0000-7000-8000-000000000c01",
};

export function capabilities(): HostCapabilities {
  return {
    os: "linux",
    desktop_backend: "wlroots",
    capture: available,
    input: available,
    input_scope: "output_bound",
    terminal: available,
    lock_session: available,
    unlock_session: available,
    speech: available,
    library: available,
    transfers: available,
    computer_use: available,
    telegram: available,
    // As on Linux, where Øphio holds a conversation's agent to what it may reach.
    conversation_access: available,
    // Alex Chen's Discord chat stays linked after the bot token was removed.
    discord: { state: "permission_missing", remedy: `${PROGRAM_NAME} channels token set discord` },
    features: ["reports_v1", "report_inbox_code_v1", "channel_link_review", "task_history", "project_folders", "launch_policy_v1", "task_branch_v1", "accounts_v1", "launchers_v1", "agent_logins_v1", "github_v1", "system_stats_v1", "collaboration_v1", "found_previews_v1", "fix_launch_v1", "agent_updates_v1", "read_only_turn_v1", "github_review_v1", "agent_preferences_v1", "github_issues_v1", "github_uat_v1", "github_run_v1", "github_close_v1", "notification_settings_v1", "queue_markup_v1", "launch_defaults_v1", "project_shown_v1", "conversations_v1", "findings_v1", "contribution_import_v1", "github_together_v1", "unarchive_task_v1", "checklist_v1", "checklist_agent_items_v1", "disable_in_ophio_v1", "review_rules_v1"],
  };
}

export function hostInfo(now: number): HostInfo {
  return {
    host_id: ids.host,
    name: "studio",
    os_label: "Arch Linux · Hyprland",
    companion_version: "0.1.0",
    inbox_path: "/home/sam/Downloads/ophio",
    boot_epoch: ids.boot,
    started_at: now - 3 * 60 * MIN,
    capabilities: capabilities(),
  };
}

const codexReadOnly = "This version of Codex cannot confirm that Ask and Plan will leave files unchanged. Update Codex on the computer to use these modes.";

/** Claude Code reports the model an alias stands for, never the alias. */
const aliasModels: Record<string, string> = { opus: "claude-opus-5-5", fable: "claude-fable-5-1" };
export const reportedModel = (id: string | null): string | null => id && (aliasModels[id] ?? id);

export const harnesses: HarnessInfo[] = [
  {
    kind: "claude_code",
    display_name: "Claude Code",
    native_version: "2.4.1",
    integration: "managed",
    installed: available,
    account: { state: "signed_in", account_label: "Personal", plan: "Max" },
    models: [
      { id: "claude-opus-5-5", label: "Claude Opus 5.5", suggested_for_computer_use: true, is_default: true },
      { id: "claude-sonnet-5", label: "Claude Sonnet 5", suggested_for_computer_use: false, is_default: false },
      { id: "claude-haiku-4-5", label: "Claude Haiku 4.5", suggested_for_computer_use: false, is_default: false },
    ],
    efforts: ["low", "medium", "high", "xhigh"],
    capabilities: {
      cancel_queued_message: available,      start_run: available,
      steer_running: available,
      interrupt: available,
      native_approvals: available,
      resume_session: available,
      child_agents: available,
      computer_use_delegation: available,
      model_selection: available,
      effort_selection: available,
    },
    modes: [
      { mode: "ask", availability: available, enforcement: "Claude Code plan mode; only Read, Glob and Grep; custom tools and permission escalation disabled" },
      { mode: "plan", availability: available, enforcement: "Claude Code plan mode; only Read, Glob and Grep; custom tools and permission escalation disabled" },
      { mode: "work", availability: available, enforcement: "Claude Code manual permissions; prompts relayed to the app" },
    ],
  },
  {
    kind: "codex",
    display_name: "Codex",
    native_version: "0.212.0",
    integration: "managed",
    installed: available,
    account: { state: "signed_in", account_label: "Personal", plan: "Pro" },
    models: [
      { id: "gpt-6-astra", label: "GPT-6-Astra", suggested_for_computer_use: true, is_default: true },
      { id: "gpt-6-mini", label: "GPT-6-Mini", suggested_for_computer_use: false, is_default: false },
    ],
    efforts: ["low", "medium", "high", "xhigh"],
    capabilities: {
      cancel_queued_message: available,      start_run: available,
      steer_running: { state: "unsupported", reason: "Codex accepts new text at the next turn boundary" },
      interrupt: available,
      native_approvals: available,
      resume_session: available,
      child_agents: { state: "unsupported", reason: "Not reported by this Codex version" },
      computer_use_delegation: available,
      model_selection: available,
      effort_selection: available,
    },
    // As an older Codex reports it: its schema can't prove a read-only run.
    modes: [
      { mode: "ask", availability: { state: "unsupported", reason: codexReadOnly }, enforcement: "This version has not confirmed that files will stay unchanged." },
      { mode: "plan", availability: { state: "unsupported", reason: codexReadOnly }, enforcement: "This version has not confirmed that files will stay unchanged." },
      { mode: "work", availability: available, enforcement: "Codex native sandbox; approval requests reviewed by the app" },
    ],
  },
  {
    kind: "pi",
    display_name: "Pi",
    native_version: null,
    integration: "terminal_only",
    installed: available,
    account: { state: "unknown", reason: "Pi does not report account state" },
    models: [],
    efforts: [],
    capabilities: {
      start_run: { state: "unsupported", reason: "Open Pi in a terminal" },
      steer_running: { state: "unsupported", reason: "Terminal only" },
      interrupt: { state: "unsupported", reason: "Terminal only" },
      native_approvals: { state: "unsupported", reason: "Terminal only" },
      resume_session: { state: "unsupported", reason: "Terminal only" },
      child_agents: { state: "unsupported", reason: "Terminal only" },
      computer_use_delegation: { state: "unsupported", reason: "Terminal only" },
      model_selection: { state: "unsupported", reason: "Terminal only" },
      effort_selection: { state: "unsupported", reason: "Terminal only" },
    },
  },
];

export function projects(now: number): Project[] {
  return [
    {
      project_id: ids.pTide,
      name: "tidewater-api",
      host_path: "/home/sam/Projects/tidewater-api",
      default_harness: "claude_code",
      default_model: "claude-opus-5-5",
      default_effort: "high",
      active_writer: ids.rFlaky,
      vcs: { kind: "git", branch: "fix/retry-flake", head: "8c41d2e", dirty_files: 3 },
      created_at: now - 40 * 24 * 60 * MIN,
    },
    {
      project_id: ids.pNotes,
      name: "field-notes",
      host_path: "/home/sam/Projects/field-notes",
      default_harness: "codex",
      default_model: "gpt-6-astra",
      default_effort: "medium",
      active_writer: null,
      vcs: { kind: "git", branch: "main", head: "a0e9f13", dirty_files: 0 },
      created_at: now - 90 * 24 * 60 * MIN,
    },
    {
      project_id: ids.pGarden,
      name: "garden-planner",
      host_path: "/home/sam/Projects/garden-planner",
      default_harness: null,
      default_model: null,
      default_effort: null,
      active_writer: null,
      vcs: null,
      created_at: now - 5 * 24 * 60 * MIN,
    },
    {
      project_id: ids.pZine,
      name: "Garden zine",
      host_path: "/home/sam/Ophio/2026-10-01-garden-zine",
      default_harness: null,
      default_model: null,
      default_effort: null,
      active_writer: null,
      vcs: null,
      created_at: now - 2 * 24 * 60 * MIN,
      one_off: true,
    },
    {
      project_id: ids.pHome,
      name: "Home",
      host_path: "/home/sam",
      default_harness: null,
      default_model: null,
      default_effort: null,
      active_writer: null,
      vcs: null,
      created_at: now - 120 * 24 * 60 * MIN,
      home: true,
    },
  ];
}

/** Each project's FINDINGS.md, as a review without GitHub left it. */
export const findings: Record<string, string> = {
  [ids.pGarden]: [
    "# Findings",
    "",
    "## [x] 1. Frost dates ignore the chosen hemisphere",
    "Severity: high",
    "Where: src/frost.ts:41",
    "The last-frost lookup always uses northern dates, so a garden in Hobart is told to plant tomatoes in July.",
    "Verdict: confirmed, reproduced — a test with a garden in Hobart was told its last frost is in October.",
    "Fixed: frost.ts reads the garden's hemisphere and shifts the table by six months; added a test for Hobart.",
    "",
    "## [ ] 2. Deleting a bed leaves its plantings behind",
    "Severity: medium",
    "Where: src/beds/store.ts:88",
    "removeBed drops the bed but not its rows in plantings, so the calendar still lists crops for a bed that's gone.",
    "Verdict: confirmed, from reading the code - removeBed deletes from beds only, and nothing else removes plantings rows with that bed_id.",
    "",
    "## [ ] 3. The seed list has no label for screen readers",
    "Severity: low",
    "Where: src/seeds/SeedList.tsx:17",
    "The list is a bare ul with icon-only buttons; VoiceOver reads each as \"button\".",
    "Verdict: rejected — each button gets an aria-label with the seed's name from SeedButton, so VoiceOver reads the names.",
    "",
  ].join("\n"),
};

/** A project's branches as its computer would list them: most recently
 * committed first, the checked-out one included. */
export function branchListing(project: Project): BranchListing | null {
  if (!project.vcs) return null;
  const others = project.project_id === ids.pTide
    ? ["main", "feature/rate-limits", "feature/webhook-retries", "chore/deps-2026-09", "release/1.4", "fix/timezone-parse", "feature/billing-export", "docs/api-examples", "spike/grpc-gateway", "release/1.3", "fix/null-cursor", "feature/audit-log"]
    : ["main", "drafts/october"];
  const current = project.vcs.branch;
  return {
    current,
    branches: current ? [current, ...others.filter((b) => b !== current)] : others,
    dirty_files: project.vcs.dirty_files,
    truncated: false,
  };
}

const device = { kind: "device", device_id: ids.device } as const;

export function tasks(now: number): Task[] {
  const items: Task[] = [
    {
      task_id: ids.tFlaky,
      project_id: ids.pTide,
      kind: "coding",
      title: "Fix the flaky retry test in the payments client",
      state: "awaiting_decision",
      revision: 7,
      harness: "claude_code",
      model: "claude-opus-5-5",
      effort: "high",
      created_by: device,
      created_at: now - 18 * MIN,
      updated_at: now - 40_000,
      current_run: ids.rFlaky,
      target_id: null,
      mission: null,
      attention_count: 0,
      status_line: "Waiting for approval to run tests serially",
    },
    {
      task_id: ids.tHeaders,
      project_id: ids.pTide,
      kind: "coding",
      title: "Add rate-limit headers to /v1/orders",
      state: "completed",
      revision: 12,
      harness: "codex",
      model: "gpt-6-astra",
      effort: "high",
      branch: { name: "ophio/rate-limit-headers", new: true },
      github: {
        link: { type: "new_pr" },
        pr: { number: 214, url: "https://github.com/samrivera/tidewater-api/pull/214", state: "open", draft: false, head: "5b7e2c9d41aa0f3e8b1c2d4e5f60718293a4b5c6", passed: 3, failed: 0, pending: 0, unresolved: 1, conflict: false },
        stage: "ready",
        rounds: 1,
        review: stamped(prReviews.corrected, now),
      },
      created_by: device,
      created_at: now - 3 * 60 * MIN,
      updated_at: now - 52 * MIN,
      current_run: null,
      target_id: null,
      mission: null,
      attention_count: 0,
      status_line: null,
    },
    {
      task_id: ids.tReadme,
      project_id: ids.pTide,
      kind: "coding",
      title: "Update README install steps for the new CLI",
      state: "accepted",
      revision: 1,
      harness: "claude_code",
      model: "claude-sonnet-5",
      effort: "medium",
      created_by: device,
      created_at: now - 25 * MIN,
      updated_at: now - 25 * MIN,
      current_run: null,
      target_id: null,
      mission: null,
      attention_count: 0,
      status_line: "Next up",
    },
    {
      task_id: ids.tDash,
      project_id: ids.pTide,
      kind: "computer_use",
      title: "Check the staging dashboard renders the new latency chart",
      state: "working",
      revision: 4,
      harness: "claude_code",
      model: "claude-opus-5-5",
      effort: "high",
      created_by: device,
      created_at: now - 6 * MIN,
      updated_at: now - 8_000,
      current_run: ids.rDash,
      target_id: ids.tgDp1,
      mission: {
        goal: "Confirm the latency chart on the staging dashboard shows p50/p95 lines for the last hour.",
        bounds: [
          { text: "Only use the Firefox window on DP-1", enforcement: "enforced" },
          { text: "Do not change dashboard settings", enforcement: "requested_only" },
        ],
        completion_checks: ["Screenshot of the chart with both lines visible"],
        stop_point: "Stop after reporting what the chart shows",
      },
      attention_count: 0,
      status_line: "Scrolling to the latency panel",
    },
    {
      // Clean, but it changes updates and signing, so it waits for the person's approval.
      task_id: ids.tSigning,
      project_id: ids.pTide,
      kind: "coding",
      title: "Sign update bundles before they're published",
      state: "completed",
      revision: 6,
      harness: "claude_code",
      model: "claude-opus-5-5",
      effort: "high",
      branch: { name: "ophio/sign-update-bundles", new: true },
      github: {
        link: { type: "new_pr" },
        pr: { number: 218, url: "https://github.com/samrivera/tidewater-api/pull/218", state: "open", draft: false, head: reviewCommits.signing, passed: 2, failed: 0, pending: 0, unresolved: 0, conflict: false },
        stage: "ready",
        rounds: 1,
        review: stamped(prReviews.approval, now),
        approval: signingApproval,
      },
      created_by: device,
      created_at: now - 5 * 60 * MIN,
      updated_at: now - 70 * MIN,
      current_run: null,
      target_id: null,
      mission: null,
      attention_count: 0,
      status_line: null,
    },
    {
      // Its corrected problem came back, so the review stopped for the person.
      task_id: ids.tExport,
      project_id: ids.pTide,
      kind: "coding",
      title: "Stream the orders export",
      state: "completed",
      revision: 9,
      harness: "codex",
      model: "gpt-6-astra",
      effort: "high",
      branch: { name: "ophio/stream-orders-export", new: true },
      github: {
        link: { type: "new_pr" },
        pr: { number: 220, url: "https://github.com/samrivera/tidewater-api/pull/220", state: "open", draft: false, head: reviewCommits.exportNow, passed: 1, failed: 1, pending: 0, unresolved: 0, conflict: false },
        stage: "ready",
        rounds: 3,
        review: stamped(prReviews.escalated, now),
      },
      created_by: device,
      created_at: now - 7 * 60 * MIN,
      updated_at: now - 90 * MIN,
      current_run: null,
      target_id: null,
      mission: null,
      attention_count: 0,
      status_line: null,
    },
    {
      task_id: ids.tNotes,
      project_id: ids.pNotes,
      kind: "coding",
      title: "Tag last week's notes by topic",
      state: "completed",
      revision: 5,
      harness: "codex",
      model: "gpt-6-astra",
      effort: "medium",
      created_by: device,
      created_at: now - 26 * 60 * MIN,
      updated_at: now - 25 * 60 * MIN,
      current_run: null,
      target_id: null,
      mission: null,
      attention_count: 0,
      status_line: null,
    },
  ];
  return items.map(task => ({ ...task, attention_count: blockingAttentionCount(attention(now), task.task_id) }));
}

export function runs(now: number): Run[] {
  return [
    {
      run_id: ids.rFlaky,
      task_id: ids.tFlaky,
      harness: "claude_code",
      requested_model: "claude-opus-5-5",
      actual_model: "claude-opus-5-5",
      requested_effort: "high",
      actual_effort: "high",
      account_label: "Personal",
      native_session: "5b2c9e1a",
      state: "awaiting_approval",
      admitted_at: now - 18 * MIN,
      dispatched_at: now - 18 * MIN + 400,
      acknowledged_at: now - 18 * MIN + 1900,
      ended_at: null,
      children: [
        {
          child_id: "0192f7a1-0000-7000-8000-000000000311",
          native_ref: "task-1",
          label: "Explore: find retry call sites",
          state: "completed",
          started_at: now - 16 * MIN,
          ended_at: now - 14 * MIN,
          provenance: "native",
        },
      ],
      outcome: null,
      holds_writer: true,
    },
    {
      run_id: ids.rHeaders,
      task_id: ids.tHeaders,
      harness: "codex",
      requested_model: "gpt-6-astra",
      actual_model: "gpt-6-astra",
      requested_effort: "high",
      actual_effort: "high",
      account_label: "Personal",
      native_session: "thr_81f2",
      state: "completed",
      admitted_at: now - 3 * 60 * MIN,
      dispatched_at: now - 3 * 60 * MIN + 300,
      acknowledged_at: now - 3 * 60 * MIN + 1200,
      ended_at: now - 52 * MIN,
      children: [],
      outcome: {
        summary:
          "Added `RateLimit-Limit`, `RateLimit-Remaining` and `RateLimit-Reset` headers to every /v1/orders response, sourced from the existing limiter. 429 responses now include `Retry-After`.",
        changed_files: [
          { path: "src/http/orders.rs", change: "modified", added: 38, removed: 6 },
          { path: "src/limiter/mod.rs", change: "modified", added: 12, removed: 2 },
          { path: "tests/orders_headers.rs", change: "added", added: 74, removed: 0 },
        ],
        checks: [
          { name: "cargo test", command: "cargo test --workspace", status: "passed", detail: "212 passed", provenance: "native" },
          { name: "clippy", command: "cargo clippy -- -D warnings", status: "passed", detail: null, provenance: "native" },
          { name: "Load test", command: null, status: "not_run", detail: "Needs the staging key", provenance: "inferred" },
        ],
        still_active: [],
        unknowns: ["Whether the gateway strips unknown headers in production"],
        usage: { input_tokens: 184_220, output_tokens: 9_804, duration_ms: 2 * 60 * MIN + 8 * MIN, reported_cost: null },
        report: { status: "done", questions: [], remaining: [], new_work: [{
          title: "Check the gateway keeps the rate-limit headers", mode: "plan",
          brief: "The orders API now sends RateLimit-Limit, RateLimit-Remaining, RateLimit-Reset and Retry-After. Check whether the production gateway passes them through or strips them, and say what to change in its config if it strips them.",
        }] },
      },
      holds_writer: false,
    },
    {
      run_id: ids.rDash,
      task_id: ids.tDash,
      harness: "claude_code",
      requested_model: "claude-opus-5-5",
      actual_model: "claude-opus-5-5",
      requested_effort: "high",
      actual_effort: "high",
      account_label: "Personal",
      native_session: "9d0e11f4",
      state: "working",
      admitted_at: now - 6 * MIN,
      dispatched_at: now - 6 * MIN + 300,
      acknowledged_at: now - 6 * MIN + 1500,
      ended_at: null,
      children: [],
      outcome: null,
      holds_writer: false,
    },
  ];
}

export type TimelineSeed = { runId: string | null; ago: number; body: TimelineBody };

export function timelines(): Record<string, TimelineSeed[]> {
  return {
    [ids.tFlaky]: [
      {
        runId: null,
        ago: 18 * MIN,
        body: {
          kind: "user_message",
          text: "The `retries_on_503` test in payments-client fails about one run in five on CI. Find out why and fix it properly — no sleeps.",
          bundle: null,
          delivery: "native_delivered",
        },
      },
      { runId: ids.rFlaky, ago: 18 * MIN - 2000, body: { kind: "run_started", run_id: ids.rFlaky } },
      {
        runId: ids.rFlaky,
        ago: 17 * MIN,
        body: { kind: "activity", tool: "Read", summary: "Read crates/payments-client/src/retry.rs", status: "succeeded", detail: null },
      },
      {
        runId: ids.rFlaky,
        ago: 16 * MIN,
        body: { kind: "activity", tool: "Task", summary: "Explore: find retry call sites", status: "succeeded", detail: "4 call sites, 2 use the shared clock" },
      },
      // A harness reports a command's input when it starts and its output under the same key when it ends.
      {
        runId: ids.rFlaky,
        ago: 12 * MIN + 20_000,
        body: {
          kind: "activity",
          key: "flaky-test",
          tool: "Bash",
          summary: "cargo test -p payments-client retries_on_503",
          status: "running",
          detail: JSON.stringify({ command: "cargo test -p payments-client retries_on_503" }),
        },
      },
      {
        runId: ids.rFlaky,
        ago: 12 * MIN,
        body: {
          kind: "activity",
          key: "flaky-test",
          tool: "Bash",
          summary: "cargo test -p payments-client retries_on_503",
          status: "failed",
          detail: "test retries_on_503 ... FAILED\n  expected 3 attempts, saw 2\n  (backoff elapsed before mock server registered)",
        },
      },
      {
        runId: ids.rFlaky,
        ago: 9 * MIN,
        body: {
          kind: "agent_message",
          text:
            "Found it. The test uses the real `Instant` clock, while the mock server registers its handler on a separate task. When the machine is busy the first backoff (50 ms) elapses before the handler exists, so one attempt is lost.\n\nFix: inject the existing `Clock` trait into `RetryPolicy` and drive it with `MockClock` in the test, so backoff advances only when the test says so. No sleeps.\n\nI've made the change. I want to run the payments tests serially to confirm there's no other shared state.",
          partial: false,
        },
      },
      {
        runId: ids.rFlaky,
        ago: 8 * MIN,
        body: { kind: "activity", tool: "Edit", summary: "Edited src/retry.rs, tests/retry.rs", status: "succeeded", detail: "+41 −17" },
      },
      { runId: ids.rFlaky, ago: 40_000, body: { kind: "approval", approval_id: ids.aTest } },
    ],
    [ids.tHeaders]: [
      {
        runId: null,
        ago: 3 * 60 * MIN,
        body: {
          kind: "user_message",
          text: "Add standard rate-limit headers to /v1/orders responses, and Retry-After on 429s. Add tests.",
          bundle: null,
          delivery: "native_delivered",
        },
      },
      { runId: ids.rHeaders, ago: 3 * 60 * MIN - 1000, body: { kind: "run_started", run_id: ids.rHeaders } },
      {
        runId: ids.rHeaders,
        ago: 2 * 60 * MIN,
        body: { kind: "activity", tool: "shell", summary: "cargo test --workspace", status: "succeeded", detail: "212 passed" },
      },
      {
        runId: ids.rHeaders,
        ago: 53 * MIN,
        body: {
          kind: "agent_message",
          text: "Done. Headers are added on every /v1/orders response and 429s carry `Retry-After`. I couldn't run the load test — it needs the staging key.",
          partial: false,
        },
      },
      { runId: ids.rHeaders, ago: 52 * MIN, body: { kind: "run_ended", run_id: ids.rHeaders, state: "completed" } },
    ],
    [ids.tDash]: [
      {
        runId: null,
        ago: 6 * MIN,
        body: {
          kind: "user_message",
          text: "Open the staging dashboard in Firefox and check the new latency chart shows p50 and p95 for the last hour.",
          bundle: null,
          delivery: "native_delivered",
        },
      },
      { runId: ids.rDash, ago: 6 * MIN - 1500, body: { kind: "run_started", run_id: ids.rDash } },
      {
        runId: ids.rDash,
        ago: 5 * MIN,
        body: { kind: "activity", tool: "computer.observe", summary: "Looked at DP-1", status: "succeeded", detail: null },
      },
      {
        runId: ids.rDash,
        ago: 4 * MIN,
        body: { kind: "activity", tool: "computer.pointer", summary: "Clicked “Latency” in the sidebar", status: "succeeded", detail: null },
      },
    ],
    [ids.tNotes]: [
      {
        runId: null,
        ago: 26 * 60 * MIN,
        body: { kind: "user_message", text: "Tag last week's notes by topic.", bundle: null, delivery: "native_delivered" },
      },
    ],
  };
}

export function approvals(now: number): Approval[] {
  return [
    {
      approval_id: ids.aTest,
      run_id: ids.rFlaky,
      task_id: ids.tFlaky,
      project_id: ids.pTide,
      native_request_id: "toolu_01Hc",
      ...toolApprovalCases[0]!.approval,
      kind: toolApprovalCases[0]!.approval.kind as Approval["kind"],
      raw_request: JSON.stringify(toolApprovalCases[0]!.input),
      revision: 1,
      state: "pending",
      requested_at: now - 40_000,
      expires_at: null,
      resolved_by: null,
      resolved_at: null,
    },
  ];
}

export function attention(now: number): AttentionItem[] {
  return [
    {
      attention_id: ids.atApproval,
      kind: "approval",
      title: "Run payments tests serially",
      body: "cargo test -p payments-client -- --test-threads=1",
      task_id: ids.tFlaky,
      run_id: ids.rFlaky,
      approval_id: ids.aTest,
      created_at: now - 40_000,
      resolved_at: null,
      priority: 10,
    },
    {
      attention_id: ids.atReady,
      kind: "completed",
      title: "Rate-limit headers on /v1/orders",
      body: null,
      task_id: ids.tHeaders,
      run_id: ids.rHeaders,
      approval_id: null,
      created_at: now - 52 * MIN,
      resolved_at: null,
      priority: 5,
    },
  ];
}

export function targets(): ControlTarget[] {
  return [
    {
      target_id: ids.tgDp1,
      kind: "output",
      label: "DP-1",
      native_ref: "DP-1",
      geometry: { width: 2560, height: 1440, scale: 1.25, rotation_degrees: 0, revision: 3 },
      capture: available,
      input: available,
    },
    {
      target_id: ids.tgHdmi,
      kind: "output",
      label: "HDMI-A-1",
      native_ref: "HDMI-A-1",
      geometry: { width: 1920, height: 1080, scale: 1, rotation_degrees: 0, revision: 1 },
      capture: available,
      input: available,
    },
    {
      target_id: ids.tgWin,
      kind: "window",
      label: "Firefox — Staging dashboard",
      native_ref: "0x55d1c0",
      geometry: { width: 1600, height: 1000, scale: 1.25, rotation_degrees: 0, revision: 2 },
      capture: available,
      input: { state: "unsupported", reason: "Input goes to the whole display on this backend; control DP-1 instead" },
    },
  ];
}

export function control(now: number): TargetControlView[] {
  return [
    {
      target_id: ids.tgDp1,
      input_state: "agent",
      lease: {
        lease_id: ids.lease1,
        target_id: ids.tgDp1,
        owner: { kind: "agent", run_id: ids.rDash },
        generation: 4,
        boot_epoch: ids.boot,
        state: "active",
        granted_at: now - 5 * MIN,
        renewal_window_ms: 5000,
      },
      model_observation: { observation_id: ids.obs1, run_id: ids.rDash, target_id: ids.tgDp1, state: "allowed", delivered_count: 6 },
      private_step: null,
    },
    { target_id: ids.tgHdmi, input_state: "ready", lease: null, model_observation: null, private_step: null },
    { target_id: ids.tgWin, input_state: "unavailable", lease: null, model_observation: null, private_step: null },
  ];
}

export const libraryRoots: LibraryRoot[] = [
  {
    root_id: ids.rootSkills,
    label: "Claude skills",
    host_path: "/home/sam/.claude/skills",
    kind: "skills",
    writable: true,
    project_id: null,
    harness: "claude_code",
  },
  {
    root_id: ids.rootAgents,
    label: "Claude agents",
    host_path: "/home/sam/.claude/agents",
    kind: "agents",
    writable: true,
    project_id: null,
    harness: "claude_code",
  },
  {
    root_id: ids.rootInstr,
    label: "tidewater-api",
    host_path: "/home/sam/Projects/tidewater-api",
    kind: "project",
    writable: true,
    project_id: ids.pTide,
    harness: null,
  },
  {
    root_id: ids.rootNotes,
    label: "Notes",
    host_path: "/home/sam/Notes",
    kind: "notes",
    writable: true,
    project_id: null,
    harness: null,
  },
  // Ophio's own agents and one-offs, joined to Library by the companion.
  {
    root_id: ids.rootOphio,
    label: "Ophio",
    host_path: "/home/sam/Ophio",
    kind: "notes",
    writable: true,
    project_id: null,
    harness: null,
  },
  // Agent files kept outside the person's own folders, reached one by one.
  {
    root_id: ids.rootClaude,
    label: "Claude Code",
    host_path: "/home/sam/.claude",
    kind: "agent_files",
    writable: true,
    project_id: null,
    harness: "claude_code",
    only: [
      "CLAUDE.md",
      "commands",
      "projects/-home-sam-Projects-tidewater-api/memory",
    ],
  },
];

/** Where the simulated computer's agent files are kept, as the companion
 * finds them; titles and sizes are read from the files when listed. */
export const agentFilePlaces: {
  rootId: string;
  path: string;
  group: AgentFileGroup;
  scope: string;
  projectId?: string;
}[] = [
  { rootId: ids.rootClaude, path: "CLAUDE.md", group: "instructions", scope: "Claude Code" },
  { rootId: ids.rootInstr, path: "AGENTS.md", group: "instructions", scope: "tidewater-api", projectId: ids.pTide },
  { rootId: ids.rootInstr, path: "CLAUDE.md", group: "instructions", scope: "tidewater-api", projectId: ids.pTide },
  { rootId: ids.rootAgents, path: "reviewer.md", group: "agents", scope: "Claude Code" },
  { rootId: ids.rootAgents, path: "docs-writer.md", group: "agents", scope: "Claude Code" },
  { rootId: ids.rootAgents, path: "security-auditor.md", group: "agents", scope: "Claude Code" },
  { rootId: ids.rootOphio, path: "Agents/video-producer.md", group: "agents", scope: "Ophio" },
  { rootId: ids.rootSkills, path: "design-review/SKILL.md", group: "skills", scope: "Claude Code" },
  { rootId: ids.rootSkills, path: "flaky-tests/SKILL.md", group: "skills", scope: "Claude Code" },
  { rootId: ids.rootSkills, path: "release-notes/SKILL.md", group: "skills", scope: "Claude Code" },
  { rootId: ids.rootClaude, path: "commands/changelog.md", group: "commands", scope: "Claude Code" },
  { rootId: ids.rootClaude, path: "projects/-home-sam-Projects-tidewater-api/memory/MEMORY.md", group: "memory", scope: "tidewater-api", projectId: ids.pTide },
  { rootId: ids.rootClaude, path: "projects/-home-sam-Projects-tidewater-api/memory/payments.md", group: "memory", scope: "tidewater-api", projectId: ids.pTide },
  { rootId: ids.rootClaude, path: "plugins/demo/skills/sample/SKILL.md", group: "skills", scope: "Demo plugin" },
];

export const libraryFiles: Record<string, Record<string, string>> = {
  [ids.rootSkills]: {
    "release-notes/SKILL.md":
      "---\nname: release-notes\ndescription: Draft release notes from merged pull requests since the last tag.\n---\n\n# Release notes\n\n1. Find the last tag with `git describe --tags --abbrev=0`.\n2. List merged pull requests since that tag.\n3. Group changes under **Added**, **Changed**, **Fixed**.\n4. Keep each line under 100 characters and link the pull request.\n\nNever invent a change that is not in the log.\n",
    "flaky-tests/SKILL.md":
      "---\nname: flaky-tests\ndescription: Diagnose a flaky test without adding sleeps.\n---\n\n# Flaky tests\n\n- Reproduce under load first (`--test-threads=1` and stress runs).\n- Look for real clocks, shared ports and global state.\n- Prefer injecting a clock over waiting.\n",
    "design-review/SKILL.md":
      "---\nname: design-review\ndescription: Review a UI change against the design checklist.\n---\n\n# Design review\n\nCheck contrast, 48 px targets, text scaling to 200%, and that Stop is never hidden.\n",
  },
  [ids.rootAgents]: {
    "reviewer.md":
      "---\nname: reviewer\ndescription: Careful code reviewer focused on correctness.\n---\n\nYou review diffs for correctness first, then clarity. Cite line numbers.\n",
    "docs-writer.md":
      "---\nname: Docs writer\ndescription: Writes and trims user docs from what changed, in plain words.\nmodel: sonnet\n---\n\nWrite for someone who has never seen the code. Short sentences; one idea each.\n",
    "security-auditor.md":
      "---\nname: Security auditor\ndescription: Looks for injection, secrets in logs and missing permission checks.\nmodel: opus\neffort: high\n---\n\nReport each finding with the file, the line and how it could be exploited.\n",
  },
  [ids.rootInstr]: {
    "CLAUDE.md":
      "# tidewater-api\n\n- Rust 2024, `cargo test --workspace` must pass.\n- Never add sleeps to tests; inject `Clock`.\n- HTTP handlers live in `src/http/`, one file per resource.\n",
    "AGENTS.md": "# Agents\n\nSame rules as CLAUDE.md. Run `cargo clippy -- -D warnings` before finishing.\n",
    "README.md":
      "# tidewater-api\n\nOrders and payments API.\n\n## Install\n\n```sh\ncargo install --path cli\n```\n\n## Run\n\n```sh\ntidewater serve --config dev.toml\n```\n",
    "docs/retry-policy.md": "# Retry policy\n\nExponential backoff, base 50 ms, factor 2, max 3 attempts. Only 502/503/504 are retried.\n",
  },
  [ids.rootOphio]: {
    "Agents/video-producer.md":
      "---\nname: Video producer\ndescription: Plans, scripts and renders short product videos with the launch-video pipeline.\nharness: claude_code\nmodel: opus\neffort: high\n---\n\nYou make short product videos. Start from a one-paragraph brief, write the script, then render with the launch-video skill. Keep each video under 60 seconds.\n",
    "2026-10-01-garden-zine/README.md": "# Garden zine\n\nA one-off: an eight-page zine of the frost dates.\n",
  },
  [ids.rootClaude]: {
    "CLAUDE.md": "# Everywhere\n\n- Answer in plain English.\n- Run the tests before saying a change works.\n",
    "commands/changelog.md": "---\ndescription: Summarise what changed since the last release.\n---\n\nList the merged changes since the last tag, grouped by area.\n",
    "projects/-home-sam-Projects-tidewater-api/memory/MEMORY.md": "# Memory\n\n- Payments tests need the local Postgres on port 5433.\n- Sam prefers small pull requests.\n",
    "projects/-home-sam-Projects-tidewater-api/memory/payments.md": "# Payments\n\nUse local Postgres for payment tests.\n",
    "plugins/demo/skills/sample/SKILL.md": "---\nname: Sample plugin skill\n---\n\nDemo.\n",
  },
  [ids.rootNotes]: {
    "2026-09-26 standup.md": "# Standup — Sep 26\n\n- Payments retry flake still open\n- Dashboard latency chart shipped to staging\n",
    "ideas/garden-planner.md": "# Garden planner\n\nFrost dates by ZIP, companion planting grid, printable seed calendar.\n",
  },
};

export function terminals(now: number): TerminalInfo[] {
  return [
    {
      terminal_id: ids.term1,
      title: "tidewater-api — zsh",
      cwd: "/home/sam/Projects/tidewater-api",
      project_id: ids.pTide,
      harness: null,
      cols: 80,
      rows: 24,
      state: { state: "running" },
      created_at: now - 50 * MIN,
      last_output_at: now - 2 * MIN,
      output_offset: 0,
    },
    {
      terminal_id: ids.term2,
      title: "pi — garden-planner",
      cwd: "/home/sam/Projects/garden-planner",
      project_id: ids.pGarden,
      harness: "pi",
      cols: 80,
      rows: 24,
      state: { state: "running" },
      created_at: now - 20 * MIN,
      last_output_at: now - MIN,
      output_offset: 0,
    },
  ];
}

export function queue(now: number): QueueView {
  return {
    project_id: ids.pTide,
    revision: 3,
    paused: false,
    writer: ids.rFlaky,
    items: [
      {
        item_id: ids.qReadme,
        task_id: ids.tReadme,
        position: 0,
        depends_on: [],
        authorization: "ask_before_start",
        state: { state: "waiting" },
        added_by: device,
        added_at: now - 25 * MIN,
      },
    ],
  };
}

export function previews(now: number): PreviewEndpoint[] {
  return [
    {
      preview_id: ids.preview1,
      project_id: ids.pTide,
      label: "API docs",
      url: "https://studio.tail1234.ts.net:8443/docs",
      reachable: true,
      checked_at: now - 4 * MIN,
    },
    // Found: a dev server an agent started in the project's folder.
    {
      preview_id: "sim-preview-found",
      project_id: ids.pTide,
      label: "Tidewater · Dashboard",
      url: "http://localhost:4173/",
      reachable: true,
      checked_at: now - 2 * MIN,
      automatic: true,
    },
    {
      preview_id: "sim-preview-found-stopped",
      project_id: ids.pNotes,
      label: "localhost:4321",
      url: "http://localhost:4321/",
      reachable: false,
      checked_at: now - 6 * MIN,
      automatic: true,
    },
    // Shown by an agent whose run has ended, and its server with it.
    {
      preview_id: "sim-preview-shown-stopped",
      project_id: ids.pTide,
      label: "CRT inspection",
      url: "http://localhost:5179/",
      reachable: false,
      checked_at: now - 9 * MIN,
      start: "npx vite --port 5179",
    },
  ];
}
