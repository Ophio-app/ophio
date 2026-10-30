// SPDX-License-Identifier: Apache-2.0
import type { ControlTarget, Grants, HarnessInfo, HostCapabilities, Project, TargetControlView } from "../../protocol/types";
import { resolveAgent } from "../talk/intent";
import { availabilityText } from "../../design/Notice";
import { inputBlocker } from "./readiness";
export const COMPUTER_TOOLS = ["See the screen", "Move the pointer", "Press keys", "Type", "Scroll", "Ask you"] as const;
/** The agent computer use falls back to when the one new work goes to can't
 * use a computer: Codex. */
export const PREFERRED_COMPUTER_AGENT = "codex";
/** Codex has no specialist of its own: its run has only the computer tools. */
export const usesComputerItself = (kind: string) => kind === "codex";
/** The agent chosen for computer use until the person picks one: the one new
 * work in `project` goes to, as New task chooses it, while it can use a
 * computer; else Codex, else the first that can. */
export function preferredComputerAgent(agents: HarnessInfo[], project?: Project): HarnessInfo | undefined {
  const able = agents.filter((a) => !agentUnavailable(a));
  const usual = resolveAgent(project, agents)?.harness;
  return able.find((a) => a.kind === usual) ?? able.find((a) => a.kind === PREFERRED_COMPUTER_AGENT) ?? able[0];
}
export function agentUnavailable(agent: HarnessInfo): string | null {
  for (const availability of [agent.installed, agent.capabilities.start_run, agent.capabilities.computer_use_delegation]) {
    if (availability.state !== "available") return availabilityText(availability).detail ?? "Agent unavailable";
  }
  if (agent.account.state !== "signed_in") return agent.account.state === "signed_out" ? agent.account.remedy : agent.account.reason;
  return null;
}
export function targetUnavailable(target: ControlTarget): string | null {
  for (const availability of [target.capture, target.input]) {
    if (availability.state !== "available") return availabilityText(availability).detail ?? "Target unavailable";
  }
  return null;
}
/** Why a new computer-use task can't work on `target` now, or null. */
export function targetBlocker(caps: HostCapabilities | null, target: ControlTarget, view: TargetControlView | undefined, grants: Grants | null): string | null {
  return inputBlocker(caps, target, view, grants) ?? targetUnavailable(target) ?? (view?.private_step ? "This target is in a private step." :
    view?.input_state === "unknown" ? "Input ownership is unknown. Check control first." :
      view?.lease && ["active", "revoking", "unknown"].includes(view.lease.state) ? "Input is already held. Review control before starting another task." : null);
}
/** Where Start computer use keeps its draft for a project on this device. */
export const goalKey = (profileId: string, projectId: string) => `computer-goal.${profileId}.${projectId}`;
/** Fills in the goal an agent suggested, unless the person has a goal of
 * their own there already or one is being started. */
export function suggestGoal(profileId: string, projectId: string, goal: string) {
  const key = goalKey(profileId, projectId);
  try {
    const draft = JSON.parse(localStorage.getItem(key) ?? "null") as { goal?: unknown; attempt?: unknown } | null;
    if (draft?.attempt || (typeof draft?.goal === "string" && draft.goal.trim())) return;
    localStorage.setItem(key, JSON.stringify({ target: "", harness: "", model: "", ...draft, goal }));
  } catch { /* The goal can still be typed if local storage is unreadable. */ }
}
