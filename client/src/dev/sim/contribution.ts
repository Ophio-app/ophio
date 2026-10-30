// SPDX-License-Identifier: Apache-2.0
//
// A simulated contribution receiver, for drills and screenshots only. It
// mirrors the real receiver's codes and its order of events (check, then
// download on a separate approval, then a paused project) but downloads
// nothing and checks no digests: a result here proves the phone's handling,
// never a handoff.

import type {
  CommandResult,
  ContributionApproval,
  ContributionCapabilities,
  ContributionCode,
  ContributionContext,
  ContributionInput,
  ContributionView,
  Project,
} from "../../protocol/types";
import { uuidv7 } from "../../protocol/ids";
import type { SimHost } from "./host";

const ACQUIRE_MS = 2500;
const VERIFY_MS = 1500;
const STOP_MS = 800;

interface Op {
  view: ContributionView;
  input: ContributionInput;
  approval: ContributionApproval | null;
  active: boolean;
  command: string | null;
  timers: ReturnType<typeof setTimeout>[];
}

/** A fixture handoff: "Fixture Browser" and one task, ten minutes long. */
export function fixtureHandoff(now = Date.now(), over: { gameId?: string; commit?: string; expiresInS?: number; instructions?: string } = {}): { packetJson: string; handoffJson: string } {
  const handoff = {
    schemaVersion: 1,
    gameId: over.gameId ?? "fixture-browser",
    releaseId: "2026.10.1",
    source: { repositoryUrl: "https://source.example/fixtures/browser", commit: over.commit ?? "9f2c1d07b4e8a3c6d5f1e2b0a9c8d7e6f5a4b3c2" },
    policies: [
      { path: over.instructions ?? "AGENTS.md", sha256: "a".repeat(64), role: "instructions" },
      { path: "CONTRIBUTING.md", sha256: "b".repeat(64), role: "contribution" },
      { path: "AI-POLICY.md", sha256: "c".repeat(64), role: "model-policy" },
    ],
    compatibility: { receiver: "ophio", protocolVersion: "ch-import-draft-1", requiredCapabilities: ["pinned-import"] },
    trust: { reviewRequired: true, isolatedWorkspaceRequired: true, execution: "explicit-approval" },
    task: { id: "parser-edge-case", revision: "1", url: null },
  };
  const issued = Math.floor(now / 1000);
  const packet = { schemaVersion: 1, kind: "contribution-import", packetId: `fixture-${issued}`, issuedAt: issued, expiresAt: issued + (over.expiresInS ?? 600), handoffSha256: "d".repeat(64) };
  return { packetJson: JSON.stringify(packet), handoffJson: JSON.stringify(handoff) };
}

export class ContributionSim {
  /** False reports a computer whose system can't hold contribution workspaces. */
  supported = true;
  /** One-shot results for drills: what the next check, download or finish reports. */
  next: { review?: ContributionCode; prepare?: ContributionCode; outcome?: ContributionCode } = {};
  /** Hold a download where it is until `release()`, for screenshots. */
  hold = false;
  /** How long a canceled download takes to confirm it stopped. */
  stopMs = STOP_MS;
  readonly ops = new Map<string, Op>();
  readonly #reviews = new Map<string, string>();

  constructor(private readonly host: SimHost) {}

  dispose(): void {
    for (const op of this.ops.values()) for (const t of op.timers) clearTimeout(t);
  }

  capabilities(): ContributionCapabilities {
    return {
      contract: "ch-import-draft-1", transport: "authenticated-control-v1", host_id: this.host.host.host_id, companion_version: this.host.host.companion_version,
      source_revision: null, supported: this.supported, capabilities: this.supported ? ["pinned-import", "policy-preview"] : [],
      // Whole, writable Library folders, as the receiver offers them.
      root_ids: this.supported ? this.host.roots.filter((r) => r.writable && !r.only).map((r) => r.root_id) : [],
      record_bytes: 65_536, transfer_bytes: 268_435_456, expanded_bytes: 536_870_912, entries: 20_000, total_seconds: 120,
    };
  }

  policy(id: string, path: string) {
    const policy = this.#op(id).view.context?.policies.find((p) => p.path === path);
    if (!policy) throw new Error("Unknown rule");
    const text = `# ${path}\n\nFixture rule text for review.\n<script>Never execute document content.</script>\n`;
    return { policy, text, size_bytes: new TextEncoder().encode(text).length };
  }

  async review(input: ContributionInput): Promise<CommandResult> {
    // Real digests of the exact bytes, as the receiver reports them.
    const [packet, handoff] = await Promise.all([sha256(input.packet_json), sha256(input.handoff_json)]);
    const key = JSON.stringify(input);
    const known = this.#reviews.get(key);
    const op = known ? this.ops.get(known) : undefined;
    const code = this.#check(input);
    if (op?.view.code === "canceled" && op.active) return { kind: "contribution", operation: { ...op.view } };
    if (op && op.view.code !== "canceled") {
      if (!op.active) op.view = { ...op.view, code: this.next.review ?? code ?? (op.view.project_id ? op.view.code : "awaiting_local_approval") };
      delete this.next.review;
      return { kind: "contribution", operation: { ...op.view } };
    }
    const view: ContributionView = {
      operation_id: uuidv7(Date.now()), code: this.next.review ?? code ?? "awaiting_local_approval",
      packet_sha256: packet, handoff_sha256: handoff, context: context(input), project_id: null, dirty: null,
      missing_capabilities: [], interruption_pending: false, execution_authorized: false,
    };
    delete this.next.review;
    this.ops.set(view.operation_id, { view, input, approval: null, active: false, command: null, timers: [] });
    this.#reviews.set(key, view.operation_id);
    return { kind: "contribution", operation: { ...view } };
  }

  prepare(id: string, approval: ContributionApproval, command: string): CommandResult {
    const op = this.#op(id);
    const now = Date.now() / 1000;
    const ctx = op.view.context;
    const refuse = (code: ContributionCode): CommandResult => {
      if (!op.active) op.view = { ...op.view, code };
      return { kind: "contribution", operation: { ...op.view, code } };
    };
    if (op.active) return { kind: "contribution", operation: { ...op.view } };
    if (approval.checked_at > now || now - approval.checked_at > 300 || approval.valid_until <= now) return refuse("freshness_unknown");
    if (!ctx || approval.game_id !== ctx.game_id || approval.commit !== ctx.commit || approval.repository_url !== ctx.repository_url) return refuse("release_mismatch");
    const drill = this.next.prepare;
    delete this.next.prepare;
    if (drill) return refuse(drill);
    // Another revision of the same repository in the same folder: never touched.
    const other = [...this.ops.values()].find((o) => o !== op && o.view.project_id && o.input.root_id === op.input.root_id
      && o.view.context?.repository_url === ctx.repository_url && o.view.context.commit !== ctx.commit);
    if (other) return refuse("workspace_conflict");
    const same = [...this.ops.values()].find((o) => o !== op && o.view.project_id && o.input.root_id === op.input.root_id
      && o.view.context?.repository_url === ctx.repository_url && o.view.context.commit === ctx.commit);
    op.approval = approval;
    op.active = true;
    op.command = command;
    op.view = { ...op.view, code: same || op.view.project_id ? "verifying" : "acquiring", project_id: same?.view.project_id ?? op.view.project_id };
    this.#advance(op, !!same || !!op.view.project_id);
    return { kind: "contribution", operation: { ...op.view } };
  }

  release(): void {
    this.hold = false;
    for (const op of this.ops.values()) if (op.active && op.view.code === "acquiring") this.#advance(op, false);
  }

  cancel(id: string): CommandResult {
    const op = this.#op(id);
    op.approval = null;
    if (op.active) {
      for (const t of op.timers) clearTimeout(t);
      op.timers = [];
      op.view = { ...op.view, code: "canceled", interruption_pending: true };
      op.timers.push(setTimeout(() => {
        op.active = false;
        op.view = { ...op.view, interruption_pending: false };
        this.#settle(op);
      }, this.stopMs));
    } else {
      op.view = { ...op.view, code: "canceled" };
    }
    return { kind: "contribution", operation: { ...op.view } };
  }

  view(id: string): ContributionView {
    const op = this.#op(id);
    const now = Date.now() / 1000;
    // As the receiver: a stored result isn't fresh trust once the check lapses.
    if ((op.view.code === "prepared" || op.view.code === "reopened") && (!op.approval || op.approval.valid_until <= now))
      return { ...op.view, code: "freshness_unknown" };
    return { ...op.view };
  }

  #op(id: string): Op {
    const op = this.ops.get(id);
    if (!op) throw { code: "not_found", message: "contribution_operation_not_found" };
    return op;
  }

  #check(input: ContributionInput): ContributionCode | null {
    let packet: { expiresAt?: number; issuedAt?: number };
    try { packet = JSON.parse(input.packet_json) as typeof packet; JSON.parse(input.handoff_json); } catch { return "invalid_json"; }
    if (input.host_id !== this.host.host.host_id) return "host_unavailable";
    const root = this.host.roots.find((r) => r.root_id === input.root_id);
    if (!root || !root.writable || root.only) return "root_changed";
    const now = Date.now() / 1000;
    if (typeof packet.issuedAt === "number" && now < packet.issuedAt) return "not_yet_valid";
    if (typeof packet.expiresAt === "number" && now >= packet.expiresAt) return "expired";
    return null;
  }

  #advance(op: Op, verifyOnly: boolean): void {
    if (this.hold && !verifyOnly) return;
    const finish = () => {
      op.active = false;
      const outcome = this.next.outcome;
      delete this.next.outcome;
      if (outcome) { op.view = { ...op.view, code: outcome }; this.#settle(op); return; }
      const reopened = !!op.view.project_id;
      const id = op.view.project_id ?? this.#addProject(op);
      op.view = { ...op.view, code: reopened ? "reopened" : "prepared", project_id: id, dirty: false };
      this.#settle(op);
    };
    if (verifyOnly) { op.timers.push(setTimeout(finish, VERIFY_MS)); return; }
    op.timers.push(setTimeout(() => {
      if (!op.active) return;
      op.view = { ...op.view, code: "verifying" };
      op.timers.push(setTimeout(finish, VERIFY_MS));
    }, ACQUIRE_MS));
  }

  #addProject(op: Op): string {
    const root = this.host.roots.find((r) => r.root_id === op.input.root_id)!;
    const name = op.view.context?.game_id ?? "contribution";
    const p: Project = {
      project_id: uuidv7(Date.now()), name, host_path: `${root.host_path}/${name}`, default_harness: null, default_model: null, default_effort: null,
      active_writer: null, vcs: { kind: "git", branch: "contribution", head: op.view.context?.commit.slice(0, 7) ?? "0000000", dirty_files: 0 }, created_at: Date.now(),
    };
    this.host.projects = [...this.host.projects, p];
    this.host.emit({ type: "project_upserted", project: p });
    return p.project_id;
  }

  #settle(op: Op): void {
    if (!op.command) return;
    this.host.emit({ type: "command_progress", command_id: op.command, stage: "observed", detail: null, result: { kind: "contribution", operation: { ...op.view } } });
  }
}

async function sha256(text: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function context(input: ContributionInput): ContributionContext | null {
  try {
    const h = JSON.parse(input.handoff_json) as {
      gameId: string; releaseId: string; source: { repositoryUrl: string; commit: string };
      policies: ContributionContext["policies"]; task: ContributionContext["task"];
    };
    return { game_id: h.gameId, release_id: h.releaseId, repository_url: h.source.repositoryUrl, commit: h.source.commit, policies: h.policies, task: h.task ?? null };
  } catch {
    return null;
  }
}
