// SPDX-License-Identifier: Apache-2.0
import type { AgentUpdateMode, AgentUpdatesView, HarnessKind } from "../../protocol/types";

/** Synthetic update state; never contacts a registry or installs programs. */
export class AgentUpdatesSim {
  view: AgentUpdatesView = {
    enabled: false, mode: "notify", revision: 1, supported: true, checking: false,
    agents: [
      { harness: "claude_code", version: "2.4.1", installed_version: "2.4.1", latest_version: "2.4.2", update_supported: true, managed: true, released_models: ["claude-fable-5-2"], checked_at: Date.now() - 40 * 60_000, problem: null },
      { harness: "codex", version: "0.212.0", installed_version: "0.212.0", latest_version: null, update_supported: true, managed: true, released_models: [], checked_at: Date.now() - 40 * 60_000, problem: "Couldn’t reach the update site to check for a new version." },
    ],
  };
  set(enabled: boolean, revision: number | null) { this.setMode(enabled ? "automatic" : "never", revision); }
  setMode(mode: AgentUpdateMode, revision: number | null) {
    if (revision !== this.view.revision) throw { code: "stale_revision", message: "Agent updates changed. Refresh before changing them." };
    this.view = { ...this.view, enabled: mode === "automatic", mode, revision: this.view.revision + 1 };
  }
  check() {
    this.view = { ...this.view, agents: this.view.agents.map(a => ({ ...a, checked_at: Date.now(), latest_version: a.latest_version ?? a.installed_version, problem: null })) };
  }
  update(harness: HarnessKind) {
    this.check();
    this.view = { ...this.view, agents: this.view.agents.map(a => a.harness === harness ? { ...a, installed_version: a.latest_version, version: a.latest_version, managed: true } : a) };
  }
}
