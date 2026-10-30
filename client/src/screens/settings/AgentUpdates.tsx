// SPDX-License-Identifier: Apache-2.0
import { useId, useRef } from "react";
import { useCommandControl } from "../../app/commandControl";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { Age } from "../../design/Age";
import { Notice } from "../../design/Notice";
import { PRODUCT_NAME } from "../../brand";
import type { HarnessKind } from "../../protocol/types";
import { useAgentUpdates } from "./useAgentUpdates";

const NAMES: Partial<Record<HarnessKind, string>> = { claude_code: "Claude Code", codex: "Codex" };

/** Whether the computer keeps its own copies of Claude Code and Codex up to date. */
export function AgentUpdates() {
  const { session, profile } = useApp();
  const link = useLink();
  const admin = useHost((s) => s.grants?.admin ?? false);
  const { view, error, supported, reload } = useAgentUpdates();
  // The revision the last change saved, until the view shows it.
  const saved = useRef(0);
  const enabled = useCommandControl({
    kind: "instant",
    value: view?.enabled ?? false,
    describe: (e) => describeError(e, profile.name),
    send: async (next, note) => {
      const revision = Math.max(view?.revision ?? 0, saved.current);
      try {
        await session.command({ command: "set_agent_updates", enabled: next }, { label: note.label, expectedRevision: revision });
        saved.current = revision + 1;
      } finally { reload(); }
    },
  });
  const hint = useId();
  if (!supported) return null;
  const online = link.state === "online";
  const change = (next: boolean) => enabled.set(next, {
    label: next ? "Keep the agents up to date" : "Use the computer’s own agents",
    what: "whether the agents are kept up to date",
    said: next ? "The computer keeps Claude Code and Codex up to date." : "Tasks use the versions installed on the computer.",
  });
  return <article className="settings-card settings-stack">
    <label className="settings-check"><input type="checkbox" checked={enabled.value} aria-describedby={hint}
      disabled={!view?.supported || !admin || !online}
      onChange={(e) => change(e.target.checked)} />Keep Claude Code and Codex up to date</label>
    <p id={hint} className="settings-meta">{PRODUCT_NAME} keeps its own copies and updates them as new versions come out, so new models show up without you updating anything. The versions installed on the computer stay as they are.</p>
    {enabled.value && view?.agents.map((agent) => <div key={agent.harness}>
      {/* One run of words, then the age, as a screen reader reads it. */}
      <p>{`${NAMES[agent.harness] ?? agent.harness} ${agent.version ?? "· no copy yet"}${agent.checked_at != null ? " · Checked " : ""}`}
        {agent.checked_at != null && <Age at={agent.checked_at} suffix=" ago" />}</p>
      {agent.problem && <Notice tone="warn">{agent.problem}</Notice>}
    </div>)}
    {view?.checking && <p className="settings-meta" role="status">Checking for new versions…</p>}
    {view && !view.supported && <Notice tone="unknown">Claude Code and Codex don’t publish programs for this computer’s system, so tasks use the versions installed on it.</Notice>}
    {!admin && <p className="settings-meta">This device needs the Manage access permission to change this.</p>}
    {!online && <Notice tone="unknown">Offline. Connect to change this.</Notice>}
    {error && <Notice tone="unknown">{error}</Notice>}
  </article>;
}
