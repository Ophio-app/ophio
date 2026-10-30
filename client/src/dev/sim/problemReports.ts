// SPDX-License-Identifier: Apache-2.0
import type { Command, ComputerPart, CrashRecord, Query, QueryData, ReportDetail, ReportProgress, ReportsView } from "../../protocol/types";
import { uuidv7 } from "../../protocol/ids";
import { reportReference } from "../../diagnostics/reference";
import { pseudonym } from "../../diagnostics/pseudonym";
import type { SimHost } from "./host";

export const SIM_APP_BYTES = new TextEncoder().encode("Simulated Ophio app update");
export const SIM_APP_SHA = "7b44e2a28a64a0c71e6d9ed3d508d9a15bc6defdc742a6bdc57eaf55ec0d9578";
export class ProblemReportsSim {
  readonly details = new Map<string, ReportDetail>();
  view: ReportsView = { reports: [], settings: { sending: true, ring: "everyone", revision: 0 }, intake: { state: "reachable" }, crashes: [] };
  failNextAnswer = false;
  #timers: ReturnType<typeof setTimeout>[] = [];
  constructor(readonly host: SimHost) {}
  dispose() { this.#timers.forEach(clearTimeout); }
  changed() { this.view = { ...this.view, reports: [...this.details.values()].map(d => d.summary).sort((a, b) => b.created_at - a.created_at) }; this.host.emit({ type: "reports_changed" }); }
  crash(): void { this.view.crashes = [this.fixtureCrash()]; this.changed(); }
  fixtureCrash(): CrashRecord { return { at: Date.now() - 30_000, side: "computer", kind: "panic", code: "panic:runtime.rs:42", build: "sim", frames: [{ file: "runtime.rs", line: 42, function: "resume" }] }; }
  async query(q: Extract<Query, { kind: "reports" | "report" | "report_preview" }>): Promise<QueryData> {
    if (q.kind === "reports") return { kind: "reports", view: structuredClone(this.view) };
    if (q.kind === "report") return { kind: "report", detail: structuredClone(this.details.get(q.report_id) ?? null) };
    const extras: ComputerPart["extras"] = [];
    for (const e of q.extras) {
      if (e.type === "names") extras.push({ type: "names", names: [{ pseudonym: await pseudonym(q.salt, "project", this.host.projects[0]!.project_id), name: "Sample project" }] });
      else extras.push({ type: "task_transcript", task: await pseudonym(q.salt, "task", e.task_id), entries: [{ at: Date.now() - 1000, role: "person", text: "Please try the sample again." }], cut: false });
    }
    return { kind: "report_preview", part: { versions: { build: this.host.host.companion_version, os: "Simulated computer", protocol: { major: 1, minor: 0 }, features: this.host.host.capabilities.features, harnesses: [] }, trail: [{ at: Date.now() - 5000, event: { type: "service", state: "started" } }, { at: Date.now() - 3000, event: { type: "error", error: { code: "unavailable" } } }], crashes: this.view.crashes.length ? structuredClone(this.view.crashes) : [this.fixtureCrash()], extras, sending: this.view.settings.sending } };
  }
  command(c: Extract<Command, { command: "submit_report" | "answer_report_question" | "withdraw_report" | "set_report_settings" }>, revision: number | null) {
    if (c.command === "set_report_settings") {
      if (revision !== this.view.settings.revision) throw { code: "stale_revision", message: "Report settings changed. Review them and try again." };
      this.view.settings = { sending: c.sending, ring: c.ring, revision: this.view.settings.revision + 1 }; this.changed(); return null;
    }
    if (c.command === "submit_report") {
      const b = c.bundle;
      if (this.details.has(b.report_id)) return null;
      this.details.set(b.report_id, { bundle: structuredClone(b), summary: { report_id: b.report_id, reference: reportReference(b.report_id), origin: b.origin, created_at: b.created_at, signature: b.signature, words: b.words, history: [{ at: Date.now(), status: { state: "received" } }], questions: [] } });
      this.view.crashes = [];
      this.changed();
      this.later(1600, () => this.progress(b.report_id, { state: "reproduced" }));
      this.later(3400, () => this.ask(b.report_id));
      return null;
    }
    const d = this.details.get(c.report_id);
    if (!d) throw { code: "not_found", message: "This report isn’t available." };
    if (c.command === "withdraw_report") {
      this.progress(c.report_id, { state: "withdrawn" });
      // A withdrawn intake keeps no attachment. The sim keeps only the local summary.
      this.details.delete(c.report_id);
      this.view.reports = [d.summary, ...this.view.reports.filter(r => r.report_id !== c.report_id)];
      this.host.emit({ type: "reports_changed" });
    } else {
      if (this.failNextAnswer) { this.failNextAnswer = false; throw { code: "already_resolved", message: "This question changed on the computer. Review it and try again." }; }
      const q = d.summary.questions.find(q => q.question_id === c.question_id);
      if (!q || q.answer) throw { code: "already_resolved", message: "This question was already answered." };
      q.answer = c.answer;
      this.resolve(c.report_id); this.changed();
      this.later(1200, () => {
        this.host.host.capabilities = { ...this.host.host.capabilities, app_updates: [{ sha256: SIM_APP_SHA, size: SIM_APP_BYTES.length, signers: ["simulated"], version: "sim-next" }] };
        this.host.emit({ type: "capabilities_changed", capabilities: this.host.host.capabilities });
        this.progress(c.report_id, { state: "fixed", build: "sim-next" });
      });
    }
    return null;
  }
  later(ms: number, fn: () => void) { this.#timers.push(setTimeout(fn, ms)); }
  resolve(id: string) { for (const a of this.host.attention.filter(a => a.report_id === id && a.resolved_at === null)) { a.resolved_at = Date.now(); this.host.emit({ type: "attention_resolved", attention_id: a.attention_id }); } }
  progress(id: string, status: ReportProgress) {
    const d = this.details.get(id); if (!d || d.summary.history.at(-1)?.status.state === "withdrawn") return;
    d.summary = { ...d.summary, history: [...d.summary.history, { at: Date.now(), status }] };
    this.changed();
  }
  ask(id: string) {
    const d = this.details.get(id); if (!d) return;
    d.summary.questions = [{ question_id: uuidv7(), template: { template: "happened_again" }, asked_at: Date.now() }];
    this.progress(id, { state: "question" });
    const item = { attention_id: uuidv7(), kind: "question" as const, title: `Report ${d.summary.reference}`, body: null, task_id: null, run_id: null, approval_id: null, report_id: id, topic: "reports" as const, priority: 80, created_at: Date.now(), resolved_at: null };
    this.host.attention.push(item); this.host.emit({ type: "attention_upserted", item });
  }
}
