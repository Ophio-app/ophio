// SPDX-License-Identifier: Apache-2.0
import type { HarnessInfo } from "../../protocol/types";
const effortLabels: Record<string, string> = { low: "Low", medium: "Medium", high: "High", xhigh: "Extra high", max: "Maximum" };
const unavailable = { state: "unsupported" as const, reason: "Installed, not supported by this version of the app yet" };
/** The agents the computer reports, with the models and efforts each offers. */
export function simCatalog(base: HarnessInfo[]): HarnessInfo[] {
  const managed = structuredClone(base.filter((h) => h.kind === "claude_code" || h.kind === "codex"));
  for (const h of managed) {
    h.catalog = { checked_at: Date.now() - 120_000, source: h.kind === "codex" ? "Codex app-server model/list and config/read" : "Installed Claude Code's model list and user settings", availability: { state: "available" }, default_model: h.models.find((m) => m.is_default)?.id ?? null, default_effort: "high" };
    if (h.kind === "codex") h.models = [h.models[0]!, { id: "gpt-6-sol", label: "GPT-6-Sol", suggested_for_computer_use: false, is_default: false }];
    if (h.kind === "claude_code") h.models = [h.models[0]!, { id: "opus", label: "Claude Opus (latest)", suggested_for_computer_use: false, is_default: false }, { id: "fable", label: "Claude Fable (latest)", suggested_for_computer_use: false, is_default: false }];
    for (const m of h.models) m.details = { alias: !m.id.includes("-"), default_effort: m.id === "gpt-6-sol" ? "low" : "high", efforts: (m.id === "gpt-6-sol" ? ["low", "medium"] : h.efforts).map((id) => ({ id, label: effortLabels[id] ?? id })) };
  }
  const example = managed[0];
  if (!example) return [];
  return [...managed, ...(["hermes", "open_claw", "pi"] as const).map((kind): HarnessInfo => ({
    ...example, kind, display_name: kind === "hermes" ? "Hermes" : kind === "pi" ? "Pi" : "OpenClaw", native_version: kind === "open_claw" ? null : "1.0 fixture",
    integration: "not_integrated", installed: kind === "open_claw" ? { state: "unsupported", reason: "Not found on this computer" } : { state: "available" },
    account: { state: "unknown", reason: "Sign-in is not checked for an unsupported agent" }, catalog: undefined, models: [], efforts: [], modes: [],
    capabilities: { start_run: unavailable, steer_running: unavailable, interrupt: unavailable, native_approvals: unavailable, resume_session: unavailable, child_agents: unavailable, computer_use_delegation: unavailable, model_selection: unavailable, effort_selection: unavailable },
  }))];
}
