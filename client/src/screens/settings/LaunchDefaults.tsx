// SPDX-License-Identifier: Apache-2.0
import { useId } from "react";
import { Bot, Sparkles } from "lucide-react";
import { prefs } from "../../app/prefs";
import { useApp, useHost } from "../../app/context";
import { ListRow, Section } from "../../design/ListRow";
import { Notice, availabilityText } from "../../design/Notice";
import type { HarnessInfo, HarnessKind } from "../../protocol/types";
import { useStore } from "../../state/store";
import { canStart } from "../talk/intent";
import { harnessLabel } from "../work/logic";

/** Who new work goes to, and whether agents stop to ask before acting. */
export function LaunchDefaults() {
  return <><DefaultAgent /><PermissionPrompts /></>;
}

function ready(h: HarnessInfo): string {
  if (!canStart(h)) return availabilityText(h.capabilities.start_run).detail ?? "Can’t start tasks on this computer";
  if (h.account.state !== "signed_in") return h.account.state === "signed_out" ? "Signed out on this computer" : "Sign-in not confirmed on this computer";
  return "Ready on this computer";
}

function DefaultAgent() {
  const { profile } = useApp();
  const chosen = useStore(prefs, (p) => p.defaultAgent ?? null);
  const agents = useHost((s) => s.harnesses).filter((h) => h.integration === "managed");
  const set = (defaultAgent: HarnessKind | null) => prefs.set((p) => ({ ...p, defaultAgent }));
  const named = chosen ? agents.find((h) => h.kind === chosen) : undefined;
  return <Section title="Default agent" id="default-agent"><div className="settings-card settings-stack">
    <p className="muted">New tasks and computer use start with this agent, on every computer, unless a project has its own default. You can still pick another agent for any task before it starts.</p>
    <div className="group" role="radiogroup" aria-label="Default agent">
      <ListRow icon={Sparkles} title="First ready agent" subtitle="Whichever agent is signed in and ready" checked={chosen === null} onClick={() => set(null)} />
      {agents.map((h) => <ListRow key={h.kind} icon={Bot} title={h.display_name} subtitle={ready(h)} checked={chosen === h.kind} onClick={() => set(h.kind)} />)}
      {chosen && !named && <ListRow icon={Bot} title={harnessLabel(chosen)} subtitle={`Not available on ${profile.name}`} checked onClick={() => set(chosen)} />}
    </div>
    {chosen && (!named || !canStart(named) || named.account.state !== "signed_in") &&
      <Notice tone="unknown">{harnessLabel(chosen)} isn’t ready on {profile.name}, so new work there starts with the first ready agent.</Notice>}
  </div></Section>;
}

function PermissionPrompts() {
  const claude = useStore(prefs, p => p.claudeBypassPermissions);
  const codex = useStore(prefs, p => p.codexBypassPermissions);
  const hint = useId();
  return <Section title="Permission prompts" id="launch-flags"><div className="settings-card">
    <p id={hint}>Agents normally stop and ask before running commands or changing files. With these on, new tasks in Work mode go ahead without asking. Only use them for work you trust.</p>
    <label className="settings-check"><input type="checkbox" checked={claude} aria-describedby={hint}
      onChange={e => prefs.set(p => ({ ...p, claudeBypassPermissions: e.target.checked }))} />Claude Code: don’t ask first</label>
    <label className="settings-check"><input type="checkbox" checked={codex} aria-describedby={hint}
      onChange={e => prefs.set(p => ({ ...p, codexBypassPermissions: e.target.checked }))} />Codex: don’t ask first, and turn off its command sandbox</label>
    <p className="settings-meta">On by default. They apply on this phone across computers, and to work an agent starts on a computer this phone connects to, such as an issue fix. You can change them beside model and effort for each task. Turning them off doesn’t change tasks already started or drafts already set.</p>
    <p className="settings-meta">Ask and Plan stay read-only. Øphio’s device permissions, Stop and private mode are unchanged. No global Claude or Codex configuration is edited.</p>
    <details className="settings-meta"><summary>What the agents are started with</summary>
      <p style={{ overflowWrap: "anywhere" }}>Claude Code: --dangerously-skip-permissions</p>
      <p style={{ overflowWrap: "anywhere" }}>Codex: --dangerously-bypass-approvals-and-sandbox</p>
    </details>
  </div></Section>;
}
