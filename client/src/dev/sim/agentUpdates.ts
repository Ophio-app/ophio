// SPDX-License-Identifier: Apache-2.0
import type { AgentUpdatesView } from "../../protocol/types";

/** Øphio's own copies of the agents, kept up to date on the simulated computer. */
export class AgentUpdatesSim {
  view: AgentUpdatesView = {
    enabled: true, revision: 1, supported: true, checking: false,
    agents: [
      { harness: "claude_code", version: "2.4.1", checked_at: Date.now() - 40 * 60_000, problem: null },
      { harness: "codex", version: "0.212.0", checked_at: Date.now() - 40 * 60_000, problem: "Couldn't reach the update site to check for a new version." },
    ],
  };
  set(enabled: boolean, revision: number | null) {
    if (revision !== this.view.revision) throw { code: "stale_revision", message: "Agent updates changed. Refresh before changing them." };
    const now = Date.now();
    this.view = {
      ...this.view, enabled, revision: this.view.revision + 1,
      agents: enabled
        ? (["claude_code", "codex"] as const).map((harness, i) => ({ harness, version: ["2.4.1", "0.212.0"][i]!, checked_at: now, problem: null }))
        : [],
    };
  }
}
