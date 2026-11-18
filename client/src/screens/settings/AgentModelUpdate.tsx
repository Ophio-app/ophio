// SPDX-License-Identifier: Apache-2.0
import { useApp, useHost } from "../../app/context";
import { Button } from "../../design/Button";
import type { HarnessInfo } from "../../protocol/types";
import { modelName } from "../talk/catalog";
import { AGENT_UPDATE_POLICY_FEATURE, useAgentUpdates } from "./useAgentUpdates";
import { compareAgentVersions } from "./agentUpdateVersions";
import "./agentUpdates.css";

/** Release announcements are not selectable models or a promise of account access. */
export function AgentModelUpdate({ harness, leave }: { harness: HarnessInfo; leave?: () => void }) {
  const supported = useHost(s => s.capabilities?.features.includes(AGENT_UPDATE_POLICY_FEATURE) ?? false);
  return supported ? <ModelUpdate harness={harness} leave={leave} /> : null;
}
function ModelUpdate({ harness, leave }: { harness: HarnessInfo; leave?: () => void }) {
  const { ui } = useApp();
  const { view } = useAgentUpdates();
  const agent = view?.agents.find(a => a.harness === harness.kind);
  const models = agent?.released_models?.filter(id => !harness.models.some(m => m.id === id)) ?? [];
  if (!agent?.installed_version || !agent.latest_version || compareAgentVersions(agent.installed_version, agent.latest_version) !== 1 || !models.length) return null;
  return <div className="agent-model-update">
    <p className="fine">{models.map(id => modelName(id)).join(", ")} may need a newer {harness.display_name}. Availability also depends on your account.</p>
    <Button size="sm" variant="quiet" onClick={() => { leave?.(); ui.push({ screen: "settings", section: "settings-agents" }); }}>Review agent update</Button>
  </div>;
}
