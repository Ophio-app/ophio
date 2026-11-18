// SPDX-License-Identifier: Apache-2.0
import { useId, useRef, useState } from "react";
import { Button } from "../../design/Button";
import { compareAgentVersions } from "./agentUpdateVersions";
import type { AgentUpdateMode } from "../../protocol/types";
import "./agentUpdates.css";
import { useCommandControl } from "../../app/commandControl";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { Age } from "../../design/Age";
import { Notice } from "../../design/Notice";
import { PRODUCT_NAME } from "../../brand";
import type { AgentCopy, HarnessKind } from "../../protocol/types";
import { AGENT_UPDATE_POLICY_FEATURE, useAgentUpdates } from "./useAgentUpdates";

const NAMES: Partial<Record<HarnessKind, string>> = { claude_code: "Claude Code", codex: "Codex" };

/** Keep older computers on their existing, two-way setting. Settings passes
 * its own `control` and the agents that have a card there, so each of those
 * agents' update line sits in its own card instead of here. */
export function AgentUpdates({ control, inCards = [] }: { control?: AgentUpdateControl; inCards?: readonly HarnessKind[] } = {}) {
  const policy = useHost(s => s.capabilities?.features.includes(AGENT_UPDATE_POLICY_FEATURE) ?? false);
  if (!policy) return <LegacyAgentUpdates />;
  return control ? <AgentUpdatePolicy control={control} inCards={inCards} /> : <OwnAgentUpdatePolicy />;
}

function OwnAgentUpdatePolicy() {
  return <AgentUpdatePolicy control={useAgentUpdateControl()} inCards={[]} />;
}

const MODES: { value: AgentUpdateMode; label: string; detail: string }[] = [
  { value: "automatic", label: "Update automatically", detail: "Keep the agents up to date for me." },
  { value: "notify", label: "Tell me when there’s an update", detail: "Tell me about updates without installing them." },
  { value: "never", label: "Don’t check", detail: "Don’t check or update automatically." },
];

export type AgentUpdateControl = ReturnType<typeof useAgentUpdateControl>;

/** One shared state for the update setting, Check now and each agent's
 * Update now, wherever on the screen they are shown. A message about an
 * action is kept with what it was about: `check` or the agent. */
export function useAgentUpdateControl() {
  const { session, profile } = useApp();
  const link = useLink();
  const admin = useHost(s => s.grants?.admin ?? false);
  const { view, error, reload } = useAgentUpdates();
  const [action, setAction] = useState<string | null>(null);
  const [problem, setProblem] = useState<{ about: string; text: string } | null>(null);
  const [finished, setFinished] = useState<{ about: string; text: string } | null>(null);
  const busy = useRef(false);
  const online = link.state === "online";
  const allowed = !!view && admin && online;
  const perform = async (harness?: HarnessKind) => {
    if (!allowed || busy.current || view?.checking) return;
    const about = harness ?? "check";
    busy.current = true; setAction(about); setProblem(null); setFinished(null);
    try {
      await session.command(harness ? { command: "update_agent", harness } : { command: "check_agent_updates" },
        { label: harness ? `Update ${NAMES[harness] ?? harness}` : "Check agent updates" });
      setFinished({ about, text: harness ? "Update requested. Your update setting stays the same." : "Check requested." });
    } catch (e) { setProblem({ about, text: describeError(e, profile.name) }); }
    finally { busy.current = false; setAction(null); reload(); }
  };
  return { view, error, reload, admin, online, allowed, action, problem, finished, perform };
}

function AgentUpdatePolicy({ control, inCards }: { control: AgentUpdateControl; inCards: readonly HarnessKind[] }) {
  const { session, profile } = useApp();
  const { view, error, reload, admin, online, allowed, action, problem, finished, perform } = control;
  const saved = useRef(0);
  const mode = useCommandControl<AgentUpdateMode>({
    kind: "instant", value: view?.mode ?? "notify",
    describe: e => describeError(e, profile.name),
    send: async (next, note) => {
      const revision = Math.max(view?.revision ?? 0, saved.current);
      try {
        await session.command({ command: "set_agent_update_mode", mode: next }, { label: note.label, expectedRevision: revision });
        saved.current = revision + 1;
      } finally { reload(); }
    },
  });
  const name = useId();
  const checks = view?.agents.flatMap(a => a.checked_at == null ? [] : [a.checked_at]) ?? [];
  const checked = checks.length && checks.length === view?.agents.length ? Math.min(...checks) : null;
  // Messages about one agent's own update are shown in its card.
  const ownProblem = problem && !inCards.includes(problem.about as HarnessKind) ? problem.text : null;
  const ownFinished = finished && !inCards.includes(finished.about as HarnessKind) ? finished.text : null;
  return <article className="settings-card settings-stack agent-updates" aria-label="Agent updates" id="agent-updates">
    <div><h3>Agent updates</h3><p className="settings-meta">For Claude Code and Codex on {profile.name}.</p></div>
    <fieldset className="agent-updates__choices" disabled={!allowed}>
      <legend className="visually-hidden">Agent update setting</legend>
      {MODES.map(choice => <label key={choice.value} className="agent-updates__choice">
        <input type="radio" name={name} value={choice.value} checked={mode.value === choice.value}
          onChange={() => void mode.set(choice.value, { label: `Agent updates: ${choice.label}`, what: "the agent update setting", said: choice.detail })} />
        <span>{choice.label}</span>
      </label>)}
    </fieldset>
    <p className="settings-meta">Updates use {PRODUCT_NAME}’s own copies. Other installations and running tasks stay as they are.</p>
    {view?.agents.filter(agent => !inCards.includes(agent.harness)).map(agent => {
      const s = agentStatus(agent);
      return <div className="agent-updates__agent" key={agent.harness}>
        <p><strong>{NAMES[agent.harness] ?? agent.harness} {agent.installed_version ?? "· version unknown"}</strong><span className="settings-meta"> · {s.status}</span></p>
        <AgentUpdateDetails control={control} agent={agent} s={s} />
      </div>;
    })}
    {!view && !error && <p className="settings-meta" role="status">Loading agent updates…</p>}
    {view?.checking && <p className="settings-meta" role="status">Checking agent versions and finishing any requested updates…</p>}
    {ownFinished && !view?.checking && <p className="settings-meta" role="status">{ownFinished}</p>}
    <div className="agent-updates__check"><Button size="sm" variant="quiet" disabled={!allowed || !!action || view?.checking} pending={action === "check"} onClick={() => void perform()}>Check now</Button>
      {checked != null && <span className="settings-meta">Checked <Age at={checked} suffix=" ago" /></span>}</div>
    {!admin && <p className="settings-meta">This device needs the Manage access permission to change this.</p>}
    {!online && <Notice tone="unknown">Offline. Connect to check or update agents.</Notice>}
    {(error || ownProblem) && <Notice tone="unknown">{ownProblem ?? error}</Notice>}
  </article>;
}

type AgentStatus = { status: string; unchecked: boolean; available: boolean };

function agentStatus(agent: AgentCopy): AgentStatus {
  const comparison = agent.installed_version && agent.latest_version ? compareAgentVersions(agent.installed_version, agent.latest_version) : null;
  const available = !!agent.latest_version && (comparison === 1 || !agent.installed_version);
  const unchecked = agent.checked_at == null && !agent.problem;
  const status = unchecked ? "not checked yet" : !agent.latest_version ? "latest version unknown"
    : agent.problem ? `latest known ${agent.latest_version}`
    : comparison === null ? `latest ${agent.latest_version}` : comparison > 0 ? `${agent.latest_version} is out` : "up to date";
  return { status, unchecked, available };
}

/** What follows an agent's version line, wherever it is shown. */
function AgentUpdateDetails({ control, agent, s }: { control: AgentUpdateControl; agent: AgentCopy; s: AgentStatus }) {
  const { view, allowed, action, perform } = control;
  return <>
    {agent.problem ? <p className="settings-meta">{agent.problem}</p> : !s.unchecked && !agent.update_supported && <p className="settings-meta">{PRODUCT_NAME} can’t update this agent here. Use its usual installer, then check again.</p>}
    {!s.unchecked && !agent.installed_version && agent.update_supported && <p className="settings-meta">The version in use couldn’t be established.</p>}
    {agent.update_supported && s.available && <Button size="sm" variant="quiet" aria-label={`Update ${NAMES[agent.harness] ?? agent.harness} now`}
      disabled={!allowed || !!action || view?.checking} pending={action === agent.harness} onClick={() => void perform(agent.harness)}>Update now</Button>}
  </>;
}

/** An agent's update line inside its own card in Settings, below the line
 * that already gives its version. The version is said again only when the
 * one tasks start differs from it. */
export function AgentUpdateLine({ control, harness, shownVersion }: { control: AgentUpdateControl; harness: HarnessKind; shownVersion: string | null }) {
  const agent = control.view?.agents.find(a => a.harness === harness);
  if (!agent) return null;
  const s = agentStatus(agent);
  const differs = agent.installed_version && !shownVersion?.includes(agent.installed_version);
  const line = `${differs ? `Tasks use ${agent.installed_version} · ` : ""}Updates: ${s.status}`;
  const { problem, finished, view } = control;
  return <div className="agent-update-line">
    <p className="settings-meta">{line}</p>
    <AgentUpdateDetails control={control} agent={agent} s={s} />
    {finished?.about === harness && !view?.checking && <p className="settings-meta" role="status">{finished.text}</p>}
    {problem?.about === harness && <Notice tone="unknown">{problem.text}</Notice>}
  </div>;
}

function LegacyAgentUpdates() {
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
