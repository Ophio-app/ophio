// SPDX-License-Identifier: Apache-2.0
//
// The simulator's copy of crates/services/src/more_access.rs and the words
// of crates/services/src/switch_work.rs: an agent asks for more than it has,
// and the person answers one card. In a chat (or a task carried on from one)
// Allow turns on exactly what it asked for; in a Plan or Ask task the way
// forward is Switch to Work. Keep it in step with the Rust files.

import type { AccessNeed, Approval, ConversationAccess, Task, TaskMode } from "../../protocol/types";
import { switchable } from "./approvalRule";

/** The order the computer sorts needs in (the Rust enum's order). */
const ORDER: readonly AccessNeed[] = ["change_files", "internet", "local_network", "whole_computer"];

/** `needs` sorted and without repeats, as the computer keeps them. */
export function tidy(needs: readonly AccessNeed[]): AccessNeed[] {
  return ORDER.filter((n) => needs.includes(n));
}

/** As ConversationAccess::normalized: what it may change it may read, lists
 * sorted without repeats, the whole computer's write implies its read, and
 * the local network only with the internet. */
export function normalized(a: ConversationAccess): ConversationAccess {
  const read = [...new Set([...a.read, ...a.write])].sort();
  const write = [...new Set(a.write)].sort();
  return { ...a, read, write, whole_read: a.whole_read || a.whole_write, local_network: a.local_network && a.internet };
}

/** Which of `needs` `access` doesn't give a run in `project`. */
export function missing(access: ConversationAccess, project: string, needs: readonly AccessNeed[]): AccessNeed[] {
  const a = normalized(access);
  const unrestricted = a.whole_read && a.whole_write && a.internet;
  const lacking = needs.filter((need) => {
    switch (need) {
      case "change_files": return !(a.whole_write || a.write.includes(project));
      case "internet": return !a.internet;
      case "local_network": return !(a.internet && (a.local_network || unrestricted));
      case "whole_computer": return !(a.whole_read && a.whole_write);
    }
  });
  // The person's own network is reached only with the internet: asking for
  // it asks for both.
  if (lacking.includes("local_network") && !a.internet) lacking.push("internet");
  return tidy(lacking);
}

/** `access` with exactly `needs` turned on for a run in `project`, and nothing else. */
export function widen(access: ConversationAccess, project: string, needs: readonly AccessNeed[]): ConversationAccess {
  const a: ConversationAccess = { ...access, read: [...access.read], write: [...access.write] };
  for (const need of needs) {
    switch (need) {
      case "change_files": a.write.push(project); break;
      case "internet": a.internet = true; break;
      case "local_network": a.internet = true; a.local_network = true; break;
      case "whole_computer": a.whole_read = true; a.whole_write = true; break;
    }
  }
  return normalized(a);
}

/** `needs` in words, as in "change files in ophio and reach the internet". */
export function needsText(needs: readonly AccessNeed[], project: string): string {
  // The local network includes the internet: say it once.
  const local = needs.includes("local_network");
  const parts: string[] = [];
  for (const need of needs) {
    switch (need) {
      case "change_files": parts.push(`change files in ${project}`); break;
      case "internet": if (!local) parts.push("reach the internet"); break;
      case "local_network": parts.push(needs.includes("internet") ? "reach the internet and your own network" : "reach your own network"); break;
      case "whole_computer": parts.push("read and change anything on this computer"); break;
    }
  }
  if (parts.length <= 1) return parts[0] ?? "";
  return `${parts.slice(0, -1).join(", ")} and ${parts[parts.length - 1]}`;
}

/** The card's one line: "Claude Code needs to change files in ophio and reach
 * your own network to build the Mac app on the MacBook." */
export function title(agent: string, needs: readonly AccessNeed[], project: string, reason: string): string {
  const r = reason.trim().replace(/[.!]+$/, "");
  const lower = r.toLowerCase();
  const joined = ["to ", "so ", "for ", "because ", "in order "].some((p) => lower.startsWith(p)) ? ` ${r}` : r ? `: ${r}` : "";
  return `${agent} needs to ${needsText(needs, project)}${joined}.`;
}

const capitalized = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

/** What the card says Allow does: for a chat, exactly what it turns on; for a
 * read-only task, that it switches to Work. */
export function scope(task: Task, agent: string, missingNeeds: readonly AccessNeed[], project: string): string {
  const what = task.conversation ? "this chat" : "this task";
  const a = task.access;
  const free = (x: ConversationAccess) => x.whole_read && x.whole_write && x.internet;
  if (a && !free(normalized(a)) && free(widen(a, task.project_id, missingNeeds)))
    return `Lifts this ${task.conversation ? "chat" : "task"}'s limits: it can read and change anything on this computer and reach whatever the computer reaches, your own network included.`;
  return task.access != null
    ? `Turns on, for ${what}: ${capitalized(needsText(missingNeeds, project))}. Nothing else changes.`
    : `Switches this task to Work mode, in the same session: ${agent} can then change files and run commands in ${project}, as far as its own permissions allow.`;
}

/** What the card says on its face beside the agent's line (more_access.rs). */
export function accessFacts(task: Task, missingNeeds: readonly AccessNeed[]): { text: string; risky?: boolean }[] {
  const a = task.access;
  if (!a) return [{ text: "switches this task to Work mode, in the same session" }];
  const free = (x: ConversationAccess) => x.whole_read && x.whole_write && x.internet;
  if (!free(normalized(a)) && free(widen(a, task.project_id, missingNeeds)))
    return [{ text: `lifts this ${task.conversation ? "chat" : "task"}'s limits, so it reaches whatever this computer reaches, your own network included`, risky: true }];
  if (missingNeeds.includes("whole_computer")) return [{ text: "lets it read and change anything on this computer", risky: true }];
  return [];
}

/** What a run in `mode` of `task` would be asked about, of `needs`: nothing
 * when it has them already or can't be given more, as `access_request`. */
export function wanted(task: Task, mode: TaskMode, needs: readonly AccessNeed[]): AccessNeed[] {
  const tidied = tidy(needs);
  if (task.access != null) return missing(task.access, task.project_id, tidied);
  if (mode === "work") return [];
  if (!switchable(task, mode)) return [];
  return tidied;
}

/** The agent's name, as harness_name. */
export function agentName(harness: Task["harness"]): string {
  switch (harness) {
    case "claude_code": return "Claude Code";
    case "codex": return "Codex";
    case "hermes": return "Hermes";
    case "open_claw": return "OpenClaw";
    case "pi": return "Pi";
  }
}

/** The timeline note a switch to Work leaves. */
export function switchNote(conversation: boolean, agent: string, project: string, holder?: string): string {
  if (holder !== undefined)
    return `Switched to Work. Waits in Next up while “${holder}” changes ${project}, then ${agent} carries on in the same session.`;
  return conversation
    ? `Turned into a task in Work mode. ${agent} carries on in the same session and may now change ${project}.`
    : `Switched to Work mode. ${agent} carries on in the same session and may now change ${project}.`;
}

/** What the agent is told as its turn in Work starts (switch_work::agent_text). */
export function switchAgentText(conversation: boolean, stopped: boolean, approval: Approval | undefined, project: string, words?: string): string {
  const what = conversation ? "turned this conversation into a task in Work mode" : "switched this task to Work mode";
  const after = stopped ? " Ophio stopped your previous turn to do it: check what finished before starting anything again." : "";
  let text: string;
  if (approval?.access_request) {
    const reason = approval.access_request ? ` (${approval.access_request.reason})` : "";
    text = `Ophio: the person ${what} so you can do what you asked for${reason}. You may now change files and run commands in ${project}.${after} Then carry on.`;
  } else if (approval) {
    text = `Ophio: the person ${what} and allowed what you asked for: ${approval.title.replace(/\.+$/, "")}. It didn't run in your previous turn, which Ophio stopped to switch, so do it again now, then carry on.`;
  } else {
    text = `Ophio: the person ${what}. You may now change files and run commands in ${project}.${after} Then carry on with what they asked.`;
  }
  return words ? `${text}\n\n${words}` : text;
}

/** What a scripted agent asks for more of on its first turn, by what it was
 * asked: a chat about building the Mac app wants to change files and reach
 * the MacBook, and a Plan task about the changelog wants to write it. Nothing
 * for anything else, so other scripted runs are unchanged. */
export function scriptedAsk(task: Task, prompt: string): { needs: AccessNeed[]; reason: string } | null {
  const p = prompt.toLowerCase();
  if (task.conversation && p.includes("mac app"))
    return { needs: ["change_files", "local_network"], reason: "to build the Mac app on the MacBook" };
  if (!task.conversation && (task.mode ?? "work") === "plan" && p.includes("changelog"))
    return { needs: ["change_files"], reason: "to write the new entries into CHANGELOG.md" };
  return null;
}
