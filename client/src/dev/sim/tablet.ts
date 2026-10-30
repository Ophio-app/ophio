// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../../brand";
import type { SimHost } from "./host";
import type { Availability, HarnessInfo, HarnessKind, HostCapabilities, ReadinessReport } from "../../protocol/types";

const setup: Availability = { state: "permission_missing", remedy: `Install computer-mcp beside ${PRODUCT_NAME} on the computer to enable Claude permission prompts.` };

/** Deterministic setup/recovery scenarios; no native tools or inference. */
export class TabletSim {
  private original: { catalog: HarnessInfo[]; capabilities: HostCapabilities } | null = null;
  private repair = false;
  failCheck = false;
  constructor(private host: SimHost) {}
  unavailable(): void {
    this.original ??= structuredClone({ catalog: this.host.harnesses, capabilities: this.host.host.capabilities });
    this.host.harnesses = this.host.harnesses.map(h => h.integration !== "managed" ? h : {
      ...h, capabilities: { ...h.capabilities, start_run: h.kind === "claude_code" ? setup : { state: "permission_missing", remedy: "Sign in to Codex on the computer." } },
    });
    this.host.host.capabilities = { ...this.host.host.capabilities,
      capture: { state: "permission_missing", remedy: "Run ophio desktop enable on the computer to permit desktop capture and input" },
      input: { state: "permission_missing", remedy: "Run ophio desktop enable on the computer to permit desktop capture and input" },
      computer_use: setup,
    };
    this.publish();
    this.host.readiness = this.report(Date.now());
    this.host.emit({ type: "readiness_reported", report: this.host.readiness });
  }
  repairOnCheck(): void { this.repair = true; }
  /** A run found the account behind this agent tool out of usage. */
  usageLimit(kind: HarnessKind, resetsAt: number | null): void {
    this.host.harnesses = this.host.harnesses.map(h => h.kind !== kind ? h : { ...h, usage_limit: { reached_at: Date.now(), resets_at: resetsAt } });
    this.publish();
  }
  refresh(report: ReadinessReport): ReadinessReport {
    if (this.failCheck) throw { code: "native_failure", message: "The computer couldn’t finish the agent checks." };
    if (this.repair && this.original) {
      this.host.harnesses = this.original.catalog;
      this.host.host.capabilities = this.original.capabilities;
      this.original = null;
      this.repair = false;
      this.publish();
    }
    return this.original ? this.report(report.checked_at) : report;
  }
  emptyBrief(): void {
    this.host.runs = this.host.runs.map(r => ({ ...r, ended_at: null, outcome: null }));
    this.host.tasks = this.host.tasks.map(t => ({ ...t, state: "completed", current_run: null }));
    for (const run of this.host.runs) this.host.emit({ type: "run_upserted", run });
    for (const task of this.host.tasks) this.host.emit({ type: "task_upserted", task });
    this.host.attention = this.host.attention.map(a => ({ ...a, resolved_at: Date.now() }));
    for (const item of this.host.attention) this.host.emit({ type: "attention_upserted", item });
  }
  private publish(): void {
    this.host.emit({ type: "harnesses_changed", harnesses: this.host.harnesses });
    this.host.emit({ type: "capabilities_changed", capabilities: this.host.host.capabilities });
  }
  private report(at: number): ReadinessReport {
    return { checked_at: at, checks: [
      { area: "network", label: "Private network reachability", status: "unknown", detail: "Reachability depends on the device and address. Check the connection on the device.", remedy: null },
      { area: "graphics", label: "Watch the desktop", status: "not_run", detail: "Needs setup.", remedy: "Run ophio desktop enable on the computer to permit desktop capture and input" },
      // An older companion's duplicate prose exercises the view's compatibility path.
      { area: "accounts", label: "Claude Code approvals", status: "not_run", detail: setup.state === "permission_missing" ? setup.remedy : null, remedy: setup.state === "permission_missing" ? setup.remedy : null },
    ] };
  }
}
