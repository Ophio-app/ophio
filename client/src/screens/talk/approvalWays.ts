// SPDX-License-Identifier: Apache-2.0
//
// The ways forward a request card offers, and what each one sends. The
// computer works out which are possible (`Approval.actions`) by the same
// rule it admits a decision by, so the card never shows a button the
// computer would refuse: a read-only task's write request offers "Switch to
// Work and allow" rather than an Allow that fails, a chat's login or GitHub
// post offers "Turn into a task", and an agent's request for more access
// offers "Allow" in a chat and "Switch to Work" in a Plan or Ask task.
//
// A computer older than `switch_to_work_v1` sends no actions, and takes no
// `switch_to_work`: the card then offers Allow and Reject as it always did.
//
// Saying no has a word per kind of card: a request is rejected, a question
// is declined (nobody rejects a question), and an agent's request for more
// access is answered "Not now".

import { readOnlyLink } from "../../app/github";
import { refusalWord } from "../../design/approvalWords";
import type { Approval, ApprovalAction, Command, HostCapabilities, Task } from "../../protocol/types";

/** The feature a computer advertises once it takes `switch_to_work` and sends `Approval.actions`. */
export const SWITCH_TO_WORK = "switch_to_work_v1";

export function switchesToWork(capabilities: HostCapabilities | null | undefined): boolean {
  return !!capabilities?.features.includes(SWITCH_TO_WORK);
}

export interface Way {
  action: ApprovalAction;
  /** The button's words. */
  label: string;
  /** The one button drawn as the main choice. */
  primary: boolean;
}

export interface Ways {
  ways: Way[];
  /** One plain line under the buttons, when what a button does needs saying. */
  note: string | null;
}

/** Who holds the project's writer slot, when another task does. */
export interface Holder {
  /** The holding task's title. */
  title: string | null;
}

const LEGACY: Record<string, ApprovalAction[]> = {
  question: ["answer", "deny"],
  login_code: ["deny"],
};

/**
 * The buttons for `approval`, in order, and the line under them. `task` is
 * the approval's task; `holder` is another task changing the project, which
 * a switch to Work waits for.
 */
export function approvalWays(
  approval: Approval,
  { task, agent, project, holder, switches, changesTasks = true }: { task: Task | null; agent: string; project: string; holder: Holder | null; switches: boolean; changesTasks?: boolean },
): Ways {
  const offered = switches && approval.actions ? approval.actions
    : (LEGACY[approval.kind] ?? ["allow", "deny"]);
  const access = !!approval.access_request;
  // A computer that can't switch is never sent a switch, and a phone that
  // may answer requests but not change tasks can neither switch one nor give
  // a chat more access.
  const actions = offered.filter((a) => (switches || (a !== "switch_to_work" && a !== "turn_into_task"))
    && (changesTasks || (a !== "switch_to_work" && a !== "turn_into_task" && !(access && a === "allow"))));
  const no = refusalWord(approval);
  const label = (a: ApprovalAction): string => {
    switch (a) {
      case "allow": return "Allow";
      case "deny": return no;
      case "answer": return "Answer";
      case "switch_to_work": return access ? "Switch to Work" : "Switch to Work and allow";
      case "turn_into_task": return "Turn into a task";
    }
  };
  const primary = actions.find((a) => a !== "deny") ?? null;
  const ways = actions.map((action) => ({ action, label: label(action), primary: action === primary }));
  const switching = actions.includes("switch_to_work") || actions.includes("turn_into_task");
  let note: string | null = null;
  if (!changesTasks && offered.some((a) => a === "switch_to_work" || a === "turn_into_task" || (access && a === "allow"))) {
    note = `This phone may answer requests but not change tasks, so it can’t do this. ${access ? "Choose Not now" : `${no} it`}, or answer from a phone that can.`;
  } else if (switching && holder) {
    note = `${holder.title ? `“${holder.title}”` : "Another task"} is changing ${project}, so this waits its turn in Next up, then ${agent} carries on in Work.`;
  } else if (actions.includes("turn_into_task")) {
    note = `A chat can’t allow this, because it acts outside the chat’s access. As a task in Work mode, ${agent} can do it.`;
  } else if (actions.includes("switch_to_work") && !access) {
    note = `This task only reads. Switched to Work, ${agent} carries on in the same session and does this.`;
  } else if (!actions.includes("allow") && !actions.includes("answer") && !switching && approval.kind !== "login_code" && approval.actions) {
    note = `${task?.conversation ? "A chat can’t allow this." : "This can’t be allowed here."} ${access ? "Choose Not now" : `${no} it`}, and the agent finishes without it.`;
  }
  return { ways, note };
}

/** The command a switch sends, taking `approval` along when it has one. */
export function switchCommand(taskId: string, approvalId: string | null, text?: string | null): Command {
  return {
    command: "switch_to_work",
    task_id: taskId,
    ...(approvalId ? { approval_id: approvalId } : {}),
    ...(text?.trim() ? { text: text.trim() } : {}),
  };
}

/** `task` as it shows once switched: in Work, and a task rather than a chat. */
export function asSwitched(task: Task): Task {
  return {
    ...task,
    mode: "work",
    conversation: false,
    ...(task.conversation && task.access
      ? { access: { ...task.access, write: [...new Set([...task.access.write, task.project_id])].sort(), read: [...new Set([...task.access.read, task.project_id])].sort() } }
      : {}),
  };
}

/** Whether `task` can be switched to Work in one tap now: a Plan or Ask task, or a chat. */
export function canSwitch(task: Task | null | undefined): task is Task {
  if (!task || task.state === "archived" || task.state === "accepted" || task.kind !== "coding") return false;
  if (readOnlyLink(task.github?.link)) return false;
  return !!task.conversation || task.mode === "plan" || task.mode === "ask";
}
