// SPDX-License-Identifier: Apache-2.0
//
// One authenticated session with one computer: the projection, queries,
// commands and their honest delivery states.

import { FIX_LAUNCH_FEATURE, FIX_LAUNCH_UPDATE, LAUNCH_POLICY_FEATURE, LAUNCH_POLICY_UPDATE } from "../app/launchPolicy";
import { TASK_BRANCH_FEATURE, TASK_BRANCH_UPDATE } from "../app/taskBranch";
import { GITHUB_FEATURE, GITHUB_UPDATE } from "../app/githubFeature";
import { LAUNCHERS_FEATURE, LAUNCHERS_UPDATE } from "../app/launchersFeature";
import { QUEUE_MARKUP_FEATURE, QUEUE_MARKUP_UPDATE } from "../app/queueMarkupFeature";
import { HostStateStore } from "../app/screenNames";
import { reconcileCachedHistory } from "./history";
import { REPORT_INBOX_FEATURE, REPORT_LOOK_INTO_FEATURE, REPORTS_FEATURE } from "../diagnostics/constants";
import { instrumentSession } from "../diagnostics/instrumentation";
import { uuidv7 } from "../protocol/ids";
import type {
  Approval,
  Command,
  CommandReceipt,
  InputAck,
  InputEnvelope,
  ProtocolError,
  ProtocolVersion,
  Query,
  QueryData,
  Run,
  ServerFrame,
} from "../protocol/types";
import {
  applyAuthenticated,
  applyReplay,
  applyGap,
  applyJournalReset,
  applyOlderTasks,
  applySnapshot,
  applyTaskRecords,
  applyTimelinePage,
  emptyHostState,
  interruptLive,
  readsHistory,
  releaseTimeline,
  retainTimeline,
  type ProjectionFrame,
} from "../state/projection";
import { Store } from "../state/store";
import { retainedApproval, retainedDecision, retainedQuery } from "./secretAnswers";
import type { ControlLink, LinkEvent, LinkState } from "./link";

/** Phone-side delivery phase of one command. */
export type CommandPhase =
  /** Written to the link; no receipt yet. */
  | "sending"
  | "accepted"
  | "duplicate"
  | "rejected"
  /** The link dropped before a receipt; the host may or may not have it. */
  | "unknown"
  /** Confirmed after reconnect that the host never received it. */
  | "not_delivered";

export interface PendingCommand {
  id: string;
  body: Command;
  label: string;
  issuedAt: number;
  phase: CommandPhase;
  receipt: CommandReceipt | null;
  error: ProtocolError | null;
  historyId?: string;
}

export class CommandError extends Error {
  constructor(
    readonly error: ProtocolError,
    readonly command: PendingCommand | null,
  ) {
    super(error.message);
  }
}

export class NotConnectedError extends Error {
  constructor() {
    super("Not connected to the computer");
  }
}

/**
 * The command left the phone and the link dropped before the computer
 * answered. It may have been carried out; the session asks the computer about
 * it once connected again, and `commands` shows the answer.
 */
export class OutcomeUnknownError extends Error {
  constructor(readonly commandId: string) {
    super("The connection dropped before the computer answered");
  }
}

const QUERY_TIMEOUT_MS = 20_000;
/** The companion closes a control connection that says nothing for 30 s, and
 * the native link gives up on a computer that says nothing for 45 s. */
const PING_INTERVAL_MS = 15_000;
/** How long a request may go unanswered before the phone doubts an open
 * link. Most answers take well under a second; a dead link never answers,
 * and the native link waits for 45 s of silence before it gives up. */
export const ANSWER_MS = 5_000;
/** The longest the screen waits for a catch-up to finish before it shows
 * what has arrived so far. */
const HOLD_MS = 3_000;
const TIMELINE_PAGE = 80;
const TASK_PAGE = 50;
/** The native link and the companion each refuse a larger JSON frame. */
export const MAX_FRAME_BYTES = 1024 * 1024;

/** The pending approvals `data` names instead of carrying. */
function withheld(data: QueryData): string[] {
  switch (data.kind) {
    case "snapshot":
    case "run":
      return data.withheld_approvals ?? [];
    case "task":
    case "task_history":
      return data.records.withheld_approvals ?? [];
    default:
      return [];
  }
}

/** `data` with `approvals` among those it carries. */
function putBack(data: QueryData, approvals: Approval[]): QueryData {
  switch (data.kind) {
    case "snapshot":
    case "run":
      return { ...data, approvals: [...data.approvals, ...approvals] };
    case "task":
    case "task_history":
      return { ...data, records: { ...data.records, approvals: [...data.records.approvals, ...approvals] } };
    default:
      return data;
  }
}

interface Waiter<T> {
  resolve: (v: T) => void;
  reject: (e: unknown) => void;
  timer?: ReturnType<typeof setTimeout>;
}

export class HostSession {
  readonly state = new HostStateStore(emptyHostState, () => this.profileId);
  readonly link = new Store<LinkState>({ state: "idle" });
  readonly negotiated = new Store<ProtocolVersion | null>(null);
  readonly commands = new Store<Record<string, PendingCommand>>({});
  /** Whether an open link may be dead: something sent has gone unanswered
   * for ANSWER_MS, or this device reports no network. The next answer, or
   * the network coming back, clears it. */
  readonly unanswered = new Store<boolean>(false);

  #queries = new Map<string, Waiter<QueryData>>();
  /** The last large read, which the next waits for. */
  #largeReads: Promise<unknown> = Promise.resolve();
  #receipts = new Map<string, Waiter<CommandReceipt>>();
  #inputAcks = new Set<(ack: InputAck) => void>();
  #unlisten: (() => void) | null = null;
  #ping: ReturnType<typeof setInterval> | null = null;
  /** Requests on this connection still waiting for an answer, by when sent. */
  #asked = new Map<string, number>();
  #answerTimer: ReturnType<typeof setTimeout> | null = null;
  #deviceOffline = false;
  #snapshotLoaded = false;
  #generation = 0;
  #bootstrapError = false;
  #snapshotRequest: { generation: number; promise: Promise<void> } | null = null;
  #now: () => number;
  /** The phone's journal baseline. Only a rollback resets the view; an
   * ordinary restart keeps the conversation and known command outcomes. */
  #history: { id: string; head: number };
  /** Taken from authentication, never from replayed journal events. A backup
   * can preserve every stored id while losing commands and catching up. */
  #commandEpoch: string | null = null;
  /** Events and live frames received while the phone catches up, applied
   * together so the screen moves straight to where the computer is instead
   * of through every step it took on the way. */
  #held: ProjectionFrame[] = [];
  /** What each catch-up waits for: `caught_up` after a subscribe, or the
   * answer to a ping sent when the app came back to the front. */
  #holds = new Set<string>();
  #holdTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    readonly profileId: string,
    private readonly control: ControlLink,
    now: () => number = Date.now,
  ) {
    this.#now = now;
    instrumentSession(this, now);
    let history: { id: string; head: number } | undefined;
    try {
      const saved = JSON.parse(localStorage.getItem(`command-history.${profileId}`) ?? "null");
      if (saved && typeof saved.id === "string" && typeof saved.head === "number") history = { id: saved.id, head: saved.head };
    } catch { /* Without a durable baseline, old attempts remain uncertain. */ }
    this.#history = history ?? { id: uuidv7(this.#now()), head: 0 };

  }

  start(): void {
    if (this.#unlisten) return;
    this.#unlisten = this.control.listen((e) => this.#onLink(e));
    this.#deviceOffline = !navigator.onLine;
    window.addEventListener("offline", this.#networkLost);
    window.addEventListener("online", this.#networkBack);
    document.addEventListener("visibilitychange", this.#shown);
    this.#judge();
    this.control.connect();
  }

  stop(): void {
    this.#generation++;
    this.#unlisten?.();
    this.#unlisten = null;
    window.removeEventListener("offline", this.#networkLost);
    window.removeEventListener("online", this.#networkBack);
    document.removeEventListener("visibilitychange", this.#shown);
    this.#endHold();
    this.#stopPing();
    this.#asked.clear();
    this.#judge();
    this.control.disconnect();
    this.#failWaiters();
    // Its link no longer reports here; a stopped session is not connected.
    this.link.set({ state: "idle" });
  }

  reconnect(): void {
    // A refused query leaves the transport usable; native disconnect disposes it.
    if (this.#bootstrapError) {
      this.#bootstrapError = false;
      this.link.set({ state: "online", since: this.#now() });
      void this.#resume();
      return;
    }
    this.control.connect();
  }

  get online(): boolean {
    return this.link.get().state === "online";
  }

  /** The app is back in front. What the link delivered while it was away can
   * arrive in a burst; it is shown in one step once the computer answers a
   * ping sent now, which comes after everything sent before it. */
  foreground(): void {
    if (!this.online || this.#bootstrapError || [...this.#holds].some((k) => k.startsWith("ping:"))) return;
    const key = this.#sendPing();
    if (key) this.#hold(key);
  }

  #shown = (): void => {
    if (document.visibilityState === "visible") this.foreground();
  };

  /** Both the local history id and boot epoch at send time, persisted as one
   * key with pending commands and outbox attempts. Legacy id-only keys fail
   * closed, since they cannot establish which boot received the command. */
  get commandHistory(): string { return JSON.stringify([this.#history.id, this.#commandEpoch]); }

  sameCommandHistory(id: string | null | undefined): boolean {
    return this.#commandEpoch !== null && id === this.commandHistory;
  }

  #rememberHistory(): void {
    try { localStorage.setItem(`command-history.${this.profileId}`, JSON.stringify(this.#history)); }
    catch { /* Outbox attempts retain their id; losing this baseline fails closed. */ }
  }

  // ---- Queries -----------------------------------------------------------

  query<K extends QueryData["kind"]>(q: Query & { kind: K }): Promise<Extract<QueryData, { kind: K }>> {
    if (q.kind === "inbox_report_code" && !this.state.get().capabilities?.features.includes("report_inbox_code_v1")) {
      return Promise.reject(new CommandError({ code: "incompatible_version", message: "This computer needs an update to show report tests." }, null));
    }
    if (["reports", "report", "report_preview"].includes(q.kind) && !this.state.get().capabilities?.features.includes(REPORTS_FEATURE)) {
      return Promise.reject(new CommandError({ code: "incompatible_version", message: "This computer needs an update to receive reports." }, null));
    }
    return this.#request<K>(q).then((data) => this.#complete(data));
  }

  #request<K extends QueryData["kind"]>(q: Query & { kind: K }): Promise<Extract<QueryData, { kind: K }>> {
    const query_id = uuidv7(this.#now());
    // A computer that reads history names the pending approvals a snapshot
    // leaves out only for a phone that says it reads them itself.
    const query: Query = q.kind === "snapshot" && this.readsHistory ? { kind: "snapshot", reads_withheld: true } : q;
    return new Promise((resolve, reject) => {
      if (!this.control.send({ type: "query", query_id, query })) {
        reject(new NotConnectedError());
        return;
      }
      this.#ask(query_id);
      const timer = setTimeout(() => {
        this.#queries.delete(query_id);
        reject(new Error("The computer did not answer in time"));
      }, QUERY_TIMEOUT_MS);
      this.#queries.set(query_id, {
        resolve: resolve as (v: QueryData) => void,
        reject,
        timer,
      });
    });
  }

  /** A reply names the pending approvals too large to share one frame with
   * the rest. Each is read alone and put back before anyone sees the reply,
   * so every reader finds them all. One gone meanwhile is left out. */
  async #complete<D extends QueryData>(data: D): Promise<D> {
    const named = withheld(data);
    if (named.length === 0) return data;
    const read = await Promise.all(named.map((approval_id) => this.#readApproval(approval_id)));
    return putBack(data, read.filter((a): a is Approval => a !== null)) as D;
  }

  #readApproval(approval_id: string): Promise<Approval | null> {
    return this.#large({ kind: "approval", approval_id }).then(
      (r) => r.approval,
      (e: unknown) => {
        if (e instanceof CommandError && e.error.code === "not_found") return null;
        throw e;
      },
    );
  }

  /** Withheld approvals and whole runs are read one after another across the
   * session. Each reply can be large, and the computer closes a connection
   * whose replies back up faster than it can send them. Only the asking
   * waits its turn, so a reply's own withheld approvals never wait on it. */
  #large<K extends QueryData["kind"]>(q: Query & { kind: K }): Promise<Extract<QueryData, { kind: K }>> {
    const asked = this.#largeReads.then(() => this.#request<K>(q));
    this.#largeReads = asked.catch(() => undefined);
    return asked;
  }

  /** Keep the conversation's loaded pages while a screen is reading them. */
  watchTimeline(taskId: string): () => void {
    this.state.set((s) => retainTimeline(s, taskId));
    let watching = true;
    return () => {
      if (!watching) return;
      watching = false;
      this.state.set((s) => releaseTimeline(s, taskId));
    };
  }

  async loadTimeline(taskId: string, older = false): Promise<void> {
    const release = this.watchTimeline(taskId);
    try {
      const tl = this.state.get().timelines[taskId];
      // Earlier items extend only a timeline known to be whole; after a gap the
      // newest page comes first.
      const earlier = older && !!tl?.loaded && tl.items.length > 0;
      const before = earlier ? tl.items[0]!.seq : null;
      const r = await this.query({ kind: "task_timeline", task_id: taskId, before_seq: before, limit: TIMELINE_PAGE });
      this.state.set((s) => applyTimelinePage(s, taskId, r.items, r.has_more, earlier));
    } finally {
      release();
    }
  }

  /** The next page of a project's tasks older than those this phone holds. */
  async loadOlderTasks(projectId: string): Promise<void> {
    const from = this.state.get().olderTasks[projectId];
    if (!from) return;
    const r = await this.query({ kind: "task_history", project_id: projectId, from, limit: TASK_PAGE });
    this.state.set((s) => applyOlderTasks(s, projectId, from, r.records, r.next));
  }

  /** The computer reads older tasks and whole runs on request. */
  get readsHistory(): boolean {
    return readsHistory(this.state.get());
  }

  /** One task this phone does not hold, with its runs and attention. */
  async loadTask(taskId: string): Promise<void> {
    const r = await this.query({ kind: "task", task_id: taskId });
    this.state.set((s) => applyTaskRecords(s, r.records));
  }

  /** One run whole, with the diffs its agent reported. Its approvals and
   * attention, and the run itself, join what this phone holds if missing.
   * One read serves everyone asking for the run meanwhile. */
  loadRun(runId: string): Promise<Run> {
    const pending = this.#runs.get(runId);
    if (pending) return pending;
    const read = this.#large({ kind: "run", run_id: runId })
      .then((data) => this.#complete(data))
      .then((r) => {
        this.state.set((s) => applyTaskRecords(s, { tasks: [], runs: [r.run], approvals: r.approvals, attention: r.attention }));
        return r.run;
      })
      .finally(() => this.#runs.delete(runId));
    this.#runs.set(runId, read);
    return read;
  }
  readonly #runs = new Map<string, Promise<Run>>();

  async refreshHarnesses(): Promise<void> {
    const r = await this.query({ kind: "harnesses" });
    this.state.set((s) => ({ ...s, harnesses: r.harnesses }));
  }

  // ---- Commands ----------------------------------------------------------

  /**
   * Send a command. Resolves with the receipt when the host accepts (or
   * reports a duplicate); rejects with CommandError when it refuses and with
   * NotConnectedError when nothing was sent. If the link drops in between,
   * the command stays visible with phase `unknown` and the promise rejects
   * with OutcomeUnknownError.
   */
  command(
    body: Command,
    opts: { label: string; expectedRevision?: number | null; expiresInMs?: number; commandId?: string } = { label: "" },
  ): Promise<CommandReceipt> {
    if (["submit_report", "answer_report_question", "withdraw_report", "set_report_settings"].includes(body.command)
      && !this.state.get().capabilities?.features.includes(REPORTS_FEATURE)) {
      return Promise.reject(new CommandError({ code: "incompatible_version", message: "This computer needs an update to receive reports." }, null));
    }
    if (body.command === "set_report_verdict" && !this.state.get().capabilities?.features.includes(REPORT_INBOX_FEATURE)) {
      return Promise.reject(new CommandError({ code: "incompatible_version", message: "This computer needs an update to answer problem reports." }, null));
    }
    if (body.command === "look_into_report" && !this.state.get().capabilities?.features.includes(REPORT_LOOK_INTO_FEATURE)) {
      return Promise.reject(new CommandError({ code: "unsupported", message: "The report fixer isn't on for this computer." }, null));
    }
    if (body.command === "create_task" && body.launch_policy != null
      && !this.state.get().capabilities?.features.includes(LAUNCH_POLICY_FEATURE)) {
      return Promise.reject(new CommandError({ code: "incompatible_version", message: LAUNCH_POLICY_UPDATE }, null));
    }
    if (body.command === "create_task" && body.github?.type === "find" && body.github.fix_with.launch_policy != null
      && !this.state.get().capabilities?.features.includes(FIX_LAUNCH_FEATURE)) {
      return Promise.reject(new CommandError({ code: "incompatible_version", message: FIX_LAUNCH_UPDATE }, null));
    }
    if (body.command === "create_task" && body.branch != null
      && !this.state.get().capabilities?.features.includes(TASK_BRANCH_FEATURE)) {
      return Promise.reject(new CommandError({ code: "incompatible_version", message: TASK_BRANCH_UPDATE }, null));
    }
    if (body.command === "create_task" && (body.start != null || body.place != null)
      && !this.state.get().capabilities?.features.includes(LAUNCHERS_FEATURE)) {
      return Promise.reject(new CommandError({ code: "incompatible_version", message: LAUNCHERS_UPDATE }, null));
    }
    if (body.command === "create_task" && body.github != null
      && !this.state.get().capabilities?.features.includes(GITHUB_FEATURE)) {
      return Promise.reject(new CommandError({ code: "incompatible_version", message: GITHUB_UPDATE }, null));
    }
    // An older computer would keep only the words and drop the markup.
    if (body.command === "queue_continuation" && body.bundle != null
      && !this.state.get().capabilities?.features.includes(QUEUE_MARKUP_FEATURE)) {
      return Promise.reject(new CommandError({ code: "incompatible_version", message: QUEUE_MARKUP_UPDATE }, null));
    }
    // A caller that persisted its intent first passes that id, so a retry of
    // an unknown outcome is deduplicated by the host instead of repeated.
    if (this.#bootstrapError) return Promise.reject(new NotConnectedError());
    const id = opts.commandId ?? uuidv7(this.#now());
    const issuedAt = this.#now();
    const pending: PendingCommand = { id, body: withoutSecret(body, body.command === "resolve_approval" ? this.state.get().approvals[body.approval_id] : undefined), label: opts.label, issuedAt, phase: "sending", receipt: null, error: null, historyId: this.commandHistory };
    const frame = {
      type: "command" as const,
      command_id: id,
      issued_at: issuedAt,
      expected_revision: opts.expectedRevision ?? null,
      // The computer counts a deadline from when it receives the command, on
      // its own clock; the absolute one is for a computer that predates that.
      expires_at: opts.expiresInMs ? issuedAt + opts.expiresInMs : null,
      ...(opts.expiresInMs ? { expires_in_ms: opts.expiresInMs } : {}),
      body,
    };
    // Handed to the link, a frame this large would be refused there and the
    // connection dropped, leaving an outcome that looks unknown.
    const size = new TextEncoder().encode(JSON.stringify(frame)).byteLength;
    if (size > MAX_FRAME_BYTES) {
      const mb = (size / (1024 * 1024)).toFixed(1);
      return Promise.reject(new CommandError({ code: "limit_exceeded", message: `Nothing was sent: this is ${mb} MB and the computer accepts up to 1 MB at a time. Make it shorter.` }, null));
    }
    this.#rememberHistory();
    const sent = this.control.send(frame);
    if (!sent) return Promise.reject(new NotConnectedError());
    this.#ask(id);
    this.commands.set((c) => ({ ...c, [id]: pending }));
    return new Promise((resolve, reject) => this.#receipts.set(id, { resolve, reject }));
  }

  /** Fire-and-report helper for UI actions that surface errors inline. */
  commandId(receiptPromise: Promise<CommandReceipt>): Promise<string | null> {
    return receiptPromise.then(
      (r) => r.command_id,
      () => null,
    );
  }

  dismissCommand(id: string): void {
    this.commands.set((c) => {
      const next = { ...c };
      delete next[id];
      return next;
    });
  }

  // ---- Input -------------------------------------------------------------

  sendInput(env: InputEnvelope): boolean {
    return this.control.send({ type: "input", ...env });
  }

  renewLease(leaseId: string, generation: number): boolean {
    return this.control.send({ type: "renew_lease", lease_id: leaseId, generation });
  }

  onInputAck(cb: (ack: InputAck) => void): () => void {
    this.#inputAcks.add(cb);
    return () => this.#inputAcks.delete(cb);
  }

  // ---- Link handling -----------------------------------------------------

  #onLink(e: LinkEvent): void {
    switch (e.type) {
      case "state":
        if (e.state.state === "online" && this.#bootstrapError) return;
        // What arrived before the link changed is shown before the change.
        if (e.state.state !== "online") this.#endHold();
        this.link.set(e.state);
        if (e.state.state !== "online") {
          this.#bootstrapError = false;
          this.#generation++;
          this.#stopPing();
          this.#failWaiters();
          this.#asked.clear();
          this.#judge();
        }
        return;
      case "authenticated":
        this.#endHold();
        this.#bootstrapError = false;
        this.#generation++;
        this.#failWaiters();
        this.#commandEpoch = e.challenge.boot_epoch;
        this.negotiated.set(e.challenge.selected);
        this.state.set((s) => applyAuthenticated(s, e.auth, e.challenge.boot_epoch, this.#now()));
        this.#checkHead(e.auth.journal_head);
        this.#history.head = Math.max(this.#history.head, e.auth.journal_head);
        this.#rememberHistory();
        this.#asked.clear();
        this.#judge();
        this.link.set({ state: "online", since: this.#now() });
        void this.#resume();
        return;
      case "frame":
        this.#onFrame(e.frame);
    }
  }

  async #resume(): Promise<void> {
    const generation = this.#generation;
    this.#startPing();
    try {
      if (!this.#snapshotLoaded) await this.#loadSnapshot();
      else if (!this.#subscribe(this.state.get().journalHead)) throw new NotConnectedError();
    } catch (error) {
      this.#bootstrapFailed(error, generation);
      return;
    }
    if (generation !== this.#generation) return;
    void this.refreshHarnesses().catch(() => undefined);
    void this.#reconcileUnknownCommands().catch(() => undefined);
  }

  #bootstrapFailed(error: unknown, generation: number): void {
    if (generation !== this.#generation) return;
    this.#generation++;
    this.#snapshotLoaded = false;
    this.#bootstrapError = true;
    this.#endHold();
    this.#failWaiters();
    this.state.set(s => ({ ...interruptLive(s), caughtUp: false }));
    this.link.set({ state: "offline", since: this.#now(), retryAt: null,
      reason: `Could not load the computer: ${error instanceof Error ? error.message : "Reconnect to try again."}` });
  }

  #loadSnapshot(): Promise<void> {
    const generation = this.#generation;
    if (this.#snapshotRequest?.generation === generation) return this.#snapshotRequest.promise;
    const promise = this.#applyBootstrap(generation).finally(() => {
      if (this.#snapshotRequest?.promise === promise) this.#snapshotRequest = null;
    });
    this.#snapshotRequest = { generation, promise };
    return promise;
  }

  async #applyBootstrap(generation: number): Promise<void> {
    const snap = await this.query({ kind: "snapshot" });
    if (generation !== this.#generation) throw new NotConnectedError();
    // The phone follows the journal from this snapshot on. A timeline kept from
    // before it, from the offline cache or a page that arrived first, may lack
    // what the computer added in between; as after a gap, its newest page is
    // loaded again and nothing is left behind a hole.
    this.state.set((s) => {
      const next = applySnapshot(reconcileCachedHistory(s, snap), snap, this.#now());
      return { ...next, timelines: Object.fromEntries(Object.entries(next.timelines).map(([id, tl]) => [id, { ...tl, loaded: false }])) };
    });
    this.#snapshotLoaded = true;
    if (!this.#subscribe(snap.journal_head)) throw new NotConnectedError();
    // Reload timelines the person has open.
    for (const [taskId, tl] of Object.entries(this.state.get().timelines)) {
      if (!tl.loaded) void this.loadTimeline(taskId).catch(() => undefined);
    }
  }

  async #reconcileUnknownCommands(): Promise<void> {
    const unknown = Object.values(this.commands.get()).filter((c) => c.phase === "unknown");
    for (const c of unknown) {
      const r = await this.query({ kind: "command_status", command_id: c.id }).catch((error: unknown) => {
        // Lost access hides the outcome, not the original send's identity.
        // Other commands can still be reconciled under their own permissions.
        if (error instanceof CommandError && error.error.code === "permission_denied") return null;
        throw error;
      });
      if (!r) continue;
      // Sent again under the same id while the question was on its way: that
      // answer is older than the repeat and says nothing about it.
      this.#updateCommand(c.id, (p) =>
        p.phase !== "unknown" || p.issuedAt !== c.issuedAt ? p
        : r.receipt
          ? { ...p, phase: r.receipt.status === "rejected" ? "rejected" : "accepted", receipt: r.receipt, error: r.receipt.error }
          : { ...p, phase: this.sameCommandHistory(p.historyId) ? "not_delivered" : "unknown" },
      );
    }
  }

  #onFrame(f: ServerFrame): void {
    if (this.#bootstrapError && ["event", "live", "caught_up", "subscription_closed", "journal_gap"].includes(f.type)) return;
    const now = this.#now();
    switch (f.type) {
      case "receipt": {
        const { type: _t, ...receipt } = f;
        void _t;
        // A resent id the computer had refused comes back as a duplicate that
        // still carries the refusal; it was never accepted.
        const refused = receipt.status === "rejected" || (receipt.status === "duplicate" && !!receipt.error);
        const phase: CommandPhase = refused ? "rejected" : receipt.status;
        this.#answered(receipt.command_id);
        this.#updateCommand(receipt.command_id, (p) => ({ ...p, phase, receipt, error: receipt.error }));
        const w = this.#receipts.get(receipt.command_id);
        this.#receipts.delete(receipt.command_id);
        if (refused) {
          w?.reject(
            new CommandError(
              receipt.error ?? { code: "internal", message: "Rejected" },
              this.commands.get()[receipt.command_id] ?? null,
            ),
          );
        } else {
          w?.resolve(receipt);
        }
        return;
      }
      case "query_result": {
        this.#answered(f.query_id);
        const w = this.#queries.get(f.query_id);
        if (!w) return;
        this.#queries.delete(f.query_id);
        if (w.timer) clearTimeout(w.timer);
        if ("Ok" in f.result) w.resolve(retainedQuery(f.result.Ok));
        else w.reject(new CommandError(f.result.Err, null));
        return;
      }
      case "event": {
        const { type: _t, ...env } = f;
        void _t;
        if (env.event.type === "approval_upserted") {
          env.event = { ...env.event, approval: retainedApproval(env.event.approval) };
        }
        this.#held.push({ env, now });
        if (this.#holds.size === 0) this.#release();
        return;
      }
      case "live": {
        const { type: _t, ...live } = f;
        void _t;
        this.#held.push({ live, now });
        if (this.#holds.size === 0) this.#release();
        return;
      }
      case "journal_gap":
        this.#endHold();
        this.#snapshotLoaded = false;
        this.state.set((s) => applyGap(s, f.requested_from, f.oldest_retained, now));
        void this.#resume();
        return;
      case "subscription_closed":
        if (this.#snapshotRequest) return;
        // The host's per-connection queue overflowed; nothing was lost from
        // the journal. Resubscribe from what this phone has applied.
        this.#release();
        this.state.set((s) => ({ ...interruptLive(s), caughtUp: false, lastHeardAt: now }));
        this.#subscribe(Math.min(f.after_seq, this.state.get().journalHead));
        return;
      case "caught_up":
        // The whole backlog is shown at once, with the computer's head.
        this.#unhold("caught_up");
        this.#release();
        if (this.#checkHead(f.head)) {
          void this.#resume();
          return;
        }
        this.state.set((s) => ({ ...s, caughtUp: true, lastHeardAt: now }));
        return;
      case "pong":
        this.#answered(`ping:${f.nonce}`);
        this.state.set((s) => ({ ...s, lastHeardAt: now }));
        this.#unhold(`ping:${f.nonce}`);
        return;
      case "input_ack": {
        const { type: _t, ...ack } = f;
        void _t;
        for (const cb of this.#inputAcks) cb(ack);
        return;
      }
      case "fatal":
      case "challenge":
      case "authenticated":
        // Handled by the native layer; a fatal frame is followed by a link
        // state change that explains it.
        return;
    }
  }

  /** A computer whose journal ends before what this phone applied has lost or
   * replaced its records. Every later event would look like a replay and be
   * ignored, so the phone starts over from a snapshot instead. */
  #checkHead(head: number): boolean {
    if (head >= Math.max(this.state.get().journalHead, this.#history.head)) return false;
    this.#resetHistory(head);
    return true;
  }

  #resetHistory(head: number): void {
    this.#history = { ...this.#history, id: uuidv7(this.#now()), head };
    this.#rememberHistory();
    this.state.set((s) => applyJournalReset(s, head, this.#now()));
    this.#snapshotLoaded = false;
    this.commands.set((commands) => Object.fromEntries(Object.entries(commands).map(([id, c]) => {
      const stage = this.state.get().commandStages[id]?.stage;
      return [id, c.phase !== "rejected" && stage !== "observed" && stage !== "failed" ? { ...c, phase: "unknown" } : c];
    })));
  }

  #updateCommand(id: string, fn: (p: PendingCommand) => PendingCommand): void {
    this.commands.set((c) => {
      const p = c[id];
      return p ? { ...c, [id]: fn(p) } : c;
    });
  }

  #failWaiters(): void {
    for (const [, w] of this.#queries) {
      if (w.timer) clearTimeout(w.timer);
      w.reject(new NotConnectedError());
    }
    this.#queries.clear();
    for (const [id, w] of this.#receipts) {
      this.#updateCommand(id, (p) => (p.phase === "sending" ? { ...p, phase: "unknown" } : p));
      w.reject(new OutcomeUnknownError(id));
    }
    this.#receipts.clear();
  }

  #startPing(): void {
    this.#stopPing();
    this.#ping = setInterval(() => this.#sendPing(), PING_INTERVAL_MS);
  }

  #sendPing(): string | null {
    const nonce = Math.floor(Math.random() * 2 ** 31);
    if (!this.control.send({ type: "ping", nonce })) return null;
    this.#ask(`ping:${nonce}`);
    return `ping:${nonce}`;
  }

  // ---- Catching up -------------------------------------------------------

  /** The computer replays what this phone missed and then says it has caught
   * up; the replay is held until then. */
  #subscribe(afterSeq: number): boolean {
    this.#hold("caught_up");
    return this.control.send({ type: "subscribe", after_seq: afterSeq });
  }

  #hold(key: string): void {
    this.#holds.add(key);
    this.#holdTimer ??= setTimeout(this.#holdExpired, HOLD_MS);
  }

  #unhold(key: string): void {
    if (!this.#holds.delete(key) || this.#holds.size > 0) return;
    this.#endHold();
  }

  #endHold(): void {
    this.#holds.clear();
    if (this.#holdTimer) clearTimeout(this.#holdTimer);
    this.#holdTimer = null;
    this.#release();
  }

  /** A long replay is shown as far as it has come; an unanswered ping is no
   * longer waited for. */
  #holdExpired = (): void => {
    this.#holdTimer = null;
    this.#release();
    for (const key of this.#holds) if (key !== "caught_up") this.#holds.delete(key);
    if (this.#holds.size > 0) this.#holdTimer = setTimeout(this.#holdExpired, HOLD_MS);
  };

  /** Everything held, applied in the order received as one change. */
  #release(): void {
    if (this.#held.length === 0) return;
    const held = this.#held;
    this.#held = [];
    this.state.set((s) => applyReplay(s, held));
    this.#history.head = Math.max(this.#history.head, this.state.get().journalHead);
    this.#rememberHistory();
  }

  // ---- Answers -----------------------------------------------------------

  #ask(key: string): void {
    // Kept in the order asked, which is not always the order of the clock:
    // two requests can share a millisecond.
    this.#asked.delete(key);
    this.#asked.set(key, this.#now());
    this.#judge();
  }

  #answered(key: string): void {
    if (!this.#asked.has(key)) return;
    // The computer answers in the order it was asked, so whatever was asked
    // before this has been answered as well; nothing asked after it has.
    for (const k of this.#asked.keys()) {
      this.#asked.delete(k);
      if (k === key) break;
    }
    this.#judge();
  }

  /** Update `unanswered`, and look again when the oldest open request is due. */
  #judge(): void {
    if (this.#answerTimer) clearTimeout(this.#answerTimer);
    this.#answerTimer = null;
    const wait = Math.min(...this.#asked.values()) + ANSWER_MS - this.#now();
    if (Number.isFinite(wait) && wait > 0) this.#answerTimer = setTimeout(() => this.#judge(), wait);
    this.unanswered.set(this.#deviceOffline || wait <= 0);
  }

  /** The device reports no network, so an open link is in doubt at once. */
  #networkLost = (): void => {
    this.#deviceOffline = true;
    this.#judge();
  };

  /** The network is back: connect now instead of after the wait between
   * attempts, or confirm a link that still looks open. */
  #networkBack = (): void => {
    this.#deviceOffline = false;
    this.#judge();
    const { state } = this.link.get();
    if (state === "online") this.#sendPing();
    else if (state !== "idle" && state !== "failed") this.control.connect();
  };

  #stopPing(): void {
    if (this.#ping) clearInterval(this.#ping);
    this.#ping = null;
  }
}

/** What the phone keeps of a command once it has left: a key, password or
 * code typed for the computer is sent once and never held. */
export function withoutSecret(body: Command, approval?: Approval): Command {
  if (body.command === "resolve_approval") return { ...body, decision: retainedDecision(body.decision, approval) };
  if (body.command === "account" && body.action.action === "set_key") return { ...body, action: { ...body.action, key: "" } };
  if (body.command === "unlock_desktop") return { ...body, password: "" };
  if (body.command !== "agent_login") return body;
  const a = body.action;
  switch (a.action) {
    case "add":
    case "set_secret":
      return { ...body, action: { ...a, password: a.password === null ? null : "", code_key: a.code_key === null ? null : "" } };
    case "answer_code":
      return { ...body, action: { ...a, code: "" } };
    default:
      return body;
  }
}
