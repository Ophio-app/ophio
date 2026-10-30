// SPDX-License-Identifier: Apache-2.0
import type { ComputerPart, IntakeAnswer, PhoneVersions, ReportBundle, ReportExtra, ReportOrigin, ReportScreenshot, ReportSide, ReportSummary, ReportUpdate, ReportSettings } from "../protocol/types";
import type { HostSession } from "../host/session";
import type { Platform } from "../host/link";
import type { UiStore } from "../app/ui";
import type { StorageLike } from "../state/savedJson";
import { Store } from "../state/store";
import { uuidv7 } from "../protocol/ids";
import packageInfo from "../../package.json";
import { buildBundle, checkedScreenshot, cutWords, describeBundle, enforceSize, mergeComputer, type BundleSection, type ReportContextInput } from "./bundle";
import { crashes, type CrashRegistry } from "./crashes";
import { IntakeClient } from "./intakeClient";
import { MAX_STATUS_IDS, REPORTS_FEATURE } from "./constants";
import { pseudonym, reportSalt } from "./pseudonym";
import { reportReference } from "./reference";
import { SavedDiagnostics } from "./storage";
import { trail, type PhoneTrail } from "./trail";

export interface ReportDraft {
  id: string;
  reference: string;
  /** Local only. Never put the draft itself in a command or HTTP request. */
  salt: string;
  bundle: ReportBundle;
  description: BundleSection[];
  phoneOnly: boolean;
  extras: ReportExtra[];
  computerSending: boolean;
  profileId: string | null;
  /** Used to prevent a late preview from restoring a switched-off extra. */
  previewRevision: number;
  signatureSide: ReportSide;
  taskId?: string;
}
export interface QueuedReport { bundle: ReportBundle; profileId: string | null; computerSending: boolean }
export interface ReportsState { queued: QueuedReport[]; direct: ReportSummary[]; sent: Record<string, ReportBundle>; tasks: Record<string, { taskId: string; profileId: string | null }>; settings: ReportSettings; error: string | null }
type SavedReports = Omit<ReportsState, "error">;
type Session = Pick<HostSession, "profileId" | "state" | "link" | "query" | "command">;
type DirectIntake = Pick<IntakeClient, "available" | "submit" | "status" | "answer" | "withdraw" | "releases">;
export interface ReportDependencies {
  storage?: StorageLike | null;
  intake?: DirectIntake;
  trail?: PhoneTrail;
  crashes?: CrashRegistry;
  now?: () => number;
}

/** Phone storage holds about five million characters for the whole app. */
const SENT_COPY_CHARS = 1_000_000;
/** A sent copy only serves "See what was sent", so it leaves out the picture and
 * keeps the newest copies that fit, leaving room for the next report. */
function keptSent(sent: Record<string, ReportBundle>): Record<string, ReportBundle> {
  const kept: Record<string, ReportBundle> = {};
  let used = 0;
  for (const [id, bundle] of Object.entries(sent).sort(([, a], [, b]) => b.created_at - a.created_at)) {
    const { screenshot: _picture, ...copy } = bundle;
    used += JSON.stringify(copy).length;
    if (used > SENT_COPY_CHARS) break;
    kept[id] = copy;
  }
  return kept;
}

export class ReportService extends Store<ReportsState> {
  #saved: SavedDiagnostics<SavedReports>;
  #intake: DirectIntake;
  #trail: PhoneTrail;
  #crashes: CrashRegistry;
  #now: () => number;
  #session: Session | null = null;
  #store: () => unknown = () => ({});
  #phone: PhoneVersions | undefined;
  #flushing: Promise<void> | undefined;
  #polling = false;
  constructor(deps: ReportDependencies = {}) {
    const saved = new SavedDiagnostics<SavedReports>("diagnostics.reports.v1", deps.storage);
    const loaded = saved.load({ queued: [], direct: [], sent: {}, tasks: {}, settings: { sending: true, ring: "everyone", revision: 0 } });
    super({ queued: Array.isArray(loaded?.queued) ? loaded.queued : [], direct: Array.isArray(loaded?.direct) ? loaded.direct : [], sent: loaded?.sent ?? {}, tasks: loaded?.tasks ?? {}, settings: loaded?.settings ?? { sending: true, ring: "everyone", revision: 0 }, error: null });
    this.#saved = saved; this.#intake = deps.intake ?? new IntakeClient(); this.#trail = deps.trail ?? trail; this.#crashes = deps.crashes ?? crashes; this.#now = deps.now ?? Date.now;
  }
  #persist(next: ReportsState): void {
    let sent = keptSent(next.sent);
    // Review copies give way, oldest first, to anything the phone still has to send.
    while (!this.#saved.save({ queued: next.queued, direct: next.direct, sent, tasks: next.tasks, settings: next.settings })) {
      const oldest = Object.entries(sent).sort(([, a], [, b]) => a.created_at - b.created_at)[0];
      if (!oldest) throw new Error("This report couldn’t be saved on this phone. Try again.");
      const { [oldest[0]]: _dropped, ...rest } = sent;
      sent = rest;
    }
    this.set({ ...next, sent });
  }
  attach(session: Session, ui: Pick<UiStore, "get">): () => void {
    this.#session = session;
    this.#store = () => ({ host: session.state.get(), ui: ui.get() });
    const changed = () => { if (this.#reachable(session)) void this.flush(); };
    const offLink = session.link.subscribe(changed), offState = session.state.subscribe(changed);
    changed();
    return () => { offLink(); offState(); if (this.#session === session) { this.#session = null; this.#store = () => ({}); } };
  }
  async setPlatform(platform: Platform): Promise<void> {
    let info: Awaited<ReturnType<NonNullable<Platform["appInfo"]>>> | undefined;
    try { info = await platform.appInfo?.(); } catch { /* Versions remain useful without native details. */ }
    const ua = navigator.userAgent;
    const android = ua.match(/Android ([\d.]+)/), ios = ua.match(/(?:iPhone|iPad).*OS ([\d_]+)/);
    this.#phone = { build: info?.build ?? packageInfo.version, platform: platform.kind === "simulated" ? "web" : android ? "android" : ios ? "ios" : "web",
      os: android ? `Android ${android[1]}` : ios ? `iOS ${ios[1]!.replace(/_/g, ".")}` : "", model: "", protocol: info?.protocol ?? { major: 1, minor: 0 } };
    // deviceName may be a person's chosen name; it is never used as a model.
    this.#crashes.build = this.#phone.build;
  }
  #supported(session = this.#session): boolean { return !!session?.state.get().capabilities?.features.includes(REPORTS_FEATURE); }
  #reachable(session = this.#session): boolean { return this.#supported(session) && session?.link.get().state === "online"; }
  async startReport(origin: ReportOrigin, context: ReportContextInput): Promise<ReportDraft> {
    const salt = reportSalt(), id = uuidv7(this.#now());
    const session = this.#session;
    const bundle = await buildBundle({ id, now: this.#now(), salt, origin, context, phone: this.#phone, trail: this.#trail.snapshot(), store: this.#store(), crashes: await this.#crashes.collect() });
    const draft: ReportDraft = { id, reference: reportReference(id), salt, bundle, description: describeBundle(bundle), phoneOnly: true, extras: [], computerSending: true, profileId: session?.profileId ?? null, previewRevision: 0, taskId: context.task, signatureSide: context.errorSide ?? (context.command && context.error ? "computer" : "phone") };
    await this.#preview(draft);
    return draft;
  }
  async #preview(draft: ReportDraft): Promise<void> {
    const session = this.#session;
    const revision = ++draft.previewRevision;
    if (!this.#reachable(session) || session?.profileId !== draft.profileId) return;
    try {
      const result = await session.query({ kind: "report_preview", salt: draft.salt, extras: structuredClone(draft.extras) });
      if (revision !== draft.previewRevision) return;
      const allowedTasks = await Promise.all(draft.extras.flatMap(e => e.type === "task_transcript" ? [pseudonym(draft.salt, "task", e.task_id)] : []));
      if (revision !== draft.previewRevision) return;
      const part: ComputerPart = { ...result.part, extras: result.part.extras.filter(extra => extra.type === "names"
        ? draft.extras.some(e => e.type === "names") : allowedTasks.includes(extra.task)) };
      draft.bundle = enforceSize(mergeComputer(draft.bundle, part, draft.signatureSide));
      draft.phoneOnly = false;
      draft.computerSending = part.sending;
      draft.description = describeBundle(draft.bundle);
    } catch { /* The person can still review and send the phone's diagnostics. */ }
  }
  async toggleExtra(draft: ReportDraft, extra: ReportExtra, enabled?: boolean): Promise<void> {
    const present = draft.extras.some(e => JSON.stringify(e) === JSON.stringify(extra));
    const on = enabled ?? !present;
    draft.extras = draft.extras.filter(e => JSON.stringify(e) !== JSON.stringify(extra));
    if (on) draft.extras.push(extra);
    // Remove consented content immediately, even when the next preview cannot load.
    draft.bundle.extras = [];
    draft.description = describeBundle(draft.bundle);
    await this.#preview(draft);
  }
  setWords(draft: ReportDraft, words: string): void {
    if (words) draft.bundle.words = cutWords(words); else delete draft.bundle.words;
    draft.bundle = enforceSize(draft.bundle); draft.description = describeBundle(draft.bundle);
  }
  setScreenshot(draft: ReportDraft, screenshot?: ReportScreenshot): void {
    const checked = checkedScreenshot(screenshot);
    if (checked) draft.bundle.screenshot = checked; else delete draft.bundle.screenshot;
    draft.bundle = enforceSize(draft.bundle); draft.description = describeBundle(draft.bundle);
  }
  send(draft: ReportDraft): string {
    const bundle = enforceSize(draft.bundle);
    // Persist before acknowledging; retries use the same id and exact reviewed data.
    if (!this.get().queued.some(q => q.bundle.report_id === draft.id) && !this.get().direct.some(r => r.report_id === draft.id)) {
      this.#persist({ ...this.get(), queued: [...this.get().queued, { bundle, profileId: draft.profileId, computerSending: draft.computerSending }], tasks: draft.taskId ? { ...this.get().tasks, [draft.id]: { taskId: draft.taskId, profileId: draft.profileId } } : this.get().tasks, error: null });
    }
    this.#crashes.acknowledge(bundle.crashes.filter(c => c.side === "phone"));
    void this.flush();
    return draft.reference;
  }
  flush(): Promise<void> {
    if (this.#flushing) return this.#flushing;
    this.#flushing = this.#deliver().catch(() => { this.set(s => ({ ...s, error: "Your report is saved here. It will be tried again." })); }).finally(() => { this.#flushing = undefined; });
    return this.#flushing;
  }
  setSettings(settings: ReportSettings): void { this.#persist({ ...this.get(), settings }); if (settings.sending) void this.flush(); }
  async #deliver(): Promise<void> {
    if (!this.get().settings.sending) return;
    for (const queued of [...this.get().queued]) {
      if (!this.get().settings.sending) break;
      const bundle = queued.bundle;
      let sent = false;
      const session = this.#session;
      if (queued.computerSending && this.#reachable(session) && (queued.profileId === null || queued.profileId === session?.profileId)) {
        try { await session!.command({ command: "submit_report", bundle }, { label: "Send report" }); sent = true; } catch { /* The same report id makes a direct retry safe. */ }
      }
      let direct = this.get().direct;
      if (!sent && await this.#intake.available()) {
        try {
          const receipt = await this.#intake.submit(bundle);
          if (receipt.report_id !== bundle.report_id) throw new Error("Unexpected report receipt");
          const summary: ReportSummary = { report_id: bundle.report_id, reference: reportReference(bundle.report_id), created_at: bundle.created_at, origin: bundle.origin, words: bundle.words,
            signature: bundle.signature, history: [{ at: this.#now(), status: receipt.status }], questions: [], checked_at: this.#now() };
          direct = [summary, ...direct.filter(r => r.report_id !== bundle.report_id)];
          sent = true;
        } catch { /* Leave the durable queue intact. */ }
      }
      if (sent) this.#persist({ ...this.get(), direct, sent: { ...this.get().sent, [bundle.report_id]: bundle }, queued: this.get().queued.filter(q => q.bundle.report_id !== bundle.report_id), error: null });
    }
  }
  async poll(): Promise<void> {
    if (this.#polling || !this.get().direct.length) return;
    this.#polling = true;
    try {
      if (!await this.#intake.available()) return;
      const reports = [...this.get().direct];
      for (let i = 0; i < reports.length; i += MAX_STATUS_IDS) {
        const response = await this.#intake.status({ report_ids: reports.slice(i, i + MAX_STATUS_IDS).map(r => r.report_id) });
        this.#applyUpdates(response.reports, response.now);
      }
    } catch { /* Status waits quietly for the next check. */ }
    finally { this.#polling = false; }
  }
  #applyUpdates(updates: ReportUpdate[], now = this.#now()): void {
    this.#persist({ ...this.get(), direct: this.get().direct.map(r => {
      const update = updates.find(u => u.report_id === r.report_id);
      return update ? { ...r, history: update.history, questions: update.questions, checked_at: now } : r;
    }) });
  }
  async answer(value: IntakeAnswer): Promise<void> {
    if (this.get().direct.some(r => r.report_id === value.report_id)) { const r = await this.#intake.answer(value); this.#applyUpdates(r.reports, r.now); }
    else if (this.#reachable()) await this.#session!.command({ command: "answer_report_question", ...value }, { label: "Answer report question" });
    else throw new Error("Connect to the computer to answer this question.");
  }
  async withdraw(reportId: string): Promise<void> {
    if (this.#flushing) await this.#flushing;
    const queued = this.get().queued.find(q => q.bundle.report_id === reportId);
    if (queued) {
      const b = queued.bundle;
      const summary: ReportSummary = { report_id: reportId, reference: reportReference(reportId), created_at: b.created_at, origin: b.origin, signature: b.signature, history: [{ at: this.#now(), status: { state: "withdrawn" } }], questions: [] };
      const { [reportId]: _removed, ...sent } = this.get().sent;
      this.#persist({ ...this.get(), sent, queued: this.get().queued.filter(q => q !== queued), direct: [summary, ...this.get().direct] });
      return;
    }
    if (this.get().direct.some(r => r.report_id === reportId)) { const r = await this.#intake.withdraw({ report_id: reportId }); this.#applyUpdates(r.reports, r.now); }
    else if (this.#reachable()) await this.#session!.command({ command: "withdraw_report", report_id: reportId }, { label: "Withdraw report" });
    else throw new Error("Connect to the computer to take this report back.");
    const { [reportId]: _removed, ...sent } = this.get().sent;
    this.#persist({ ...this.get(), sent });
  }
  releases() { return this.#intake.releases(); }
  follow(): () => void {
    let stopped = false;
    const retry = () => { if (!stopped && document.visibilityState !== "hidden") void this.flush(); };
    const poll = () => { if (!stopped && document.visibilityState !== "hidden") void this.poll(); };
    const retryTimer = setInterval(retry, 60_000), pollTimer = setInterval(poll, 5 * 60_000);
    window.addEventListener("online", retry); document.addEventListener("visibilitychange", retry);
    retry(); poll();
    return () => { stopped = true; clearInterval(retryTimer); clearInterval(pollTimer); window.removeEventListener("online", retry); document.removeEventListener("visibilitychange", retry); };
  }
}
export const reports = new ReportService();
export const startReport = (origin: ReportOrigin, context: ReportContextInput) => reports.startReport(origin, context);
export const toggleExtra = (draft: ReportDraft, extra: ReportExtra, enabled?: boolean) => reports.toggleExtra(draft, extra, enabled);
export const setWords = (draft: ReportDraft, words: string) => reports.setWords(draft, words);
export const setScreenshot = (draft: ReportDraft, picture?: ReportScreenshot) => reports.setScreenshot(draft, picture);
export const send = (draft: ReportDraft) => reports.send(draft);
