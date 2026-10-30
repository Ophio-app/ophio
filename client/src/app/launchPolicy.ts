// SPDX-License-Identifier: Apache-2.0
import type { HarnessKind, LaunchPolicy, TaskMode } from "../protocol/types";
import { prefs } from "./prefs";

export const LAUNCH_POLICY_FEATURE = "launch_policy_v1";
export const LAUNCH_POLICY_UPDATE = "Update Øphio on this computer to choose permission prompts. Nothing is sent with a setting it can’t apply.";
/** A Find's fixes start with the launch flags chosen for them. */
export const FIX_LAUNCH_FEATURE = "fix_launch_v1";
export const FIX_LAUNCH_UPDATE = "This computer’s Øphio is older, so the fixes ask for approval as before. Update it to choose whether they ask first.";

export function defaultLaunchPolicy(harness: HarnessKind): LaunchPolicy | null {
  const p = prefs.get();
  if (harness === "claude_code") return p.claudeBypassPermissions ? "bypass_permissions" : "standard";
  if (harness === "codex") return p.codexBypassPermissions ? "bypass_permissions" : "standard";
  return null;
}

export function launchChoices(harness: HarnessKind): { value: LaunchPolicy; label: string; flags: string; detail: string }[] {
  if (harness !== "claude_code" && harness !== "codex") return [];
  return [
    { value: "bypass_permissions", label: "Skip permissions", flags: harness === "claude_code" ? "--dangerously-skip-permissions" : "--dangerously-bypass-approvals-and-sandbox", detail: `${skipRisk(harness)} Only use it for work you trust.` },
    { value: "standard", label: "Ask for approval", flags: harness === "claude_code" ? "--permission-mode manual" : "--sandbox workspace-write --ask-for-approval on-request", detail: "Restricted actions ask for your approval in Øphio." },
    ...(harness === "claude_code"
      ? [{ value: "accept_edits" as const, label: "Accept edits", flags: "--permission-mode acceptEdits", detail: "File edits are allowed automatically; restricted commands still ask." }]
      : [{ value: "sandboxed" as const, label: "Sandbox, no prompts", flags: "--sandbox workspace-write --ask-for-approval never", detail: "Work inside the command sandbox. Actions requiring approval are refused instead of asking." }]),
  ];
}

const skipRisk = (harness: HarnessKind) => harness === "codex"
  ? "It won’t ask before running commands or changing files, and runs outside its command sandbox."
  : "It won’t ask before running commands or changing files.";

/** The permission setting, in words; its flags are in launchFlags. */
export function launchSummary(harness: HarnessKind, policy: LaunchPolicy | null | undefined, mode: TaskMode = "work"): string {
  if (mode !== "work") return "Read-only: permission prompts matter only in Work. Ask and Plan keep their restrictions.";
  if (policy == null) return "The agent’s own permission settings, unchanged.";
  const choice = launchChoices(harness).find(c => c.value === policy);
  return choice ? choice.label : "This agent doesn’t support that permission setting.";
}

/** What the agent is started with on the command line, for those who look. */
export function launchFlags(harness: HarnessKind, policy: LaunchPolicy | null | undefined, mode: TaskMode = "work"): string | null {
  if (mode !== "work" || policy == null) return null;
  return launchChoices(harness).find(c => c.value === policy)?.flags ?? null;
}

/** Said whenever a setting lets the agent act without asking. */
export function launchRisk(harness: HarnessKind, policy: LaunchPolicy | null | undefined, mode: TaskMode = "work"): string | null {
  if (mode !== "work" || policy !== "bypass_permissions" || !launchChoices(harness).length) return null;
  return skipRisk(harness);
}
