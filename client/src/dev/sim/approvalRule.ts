// SPDX-License-Identifier: Apache-2.0
//
// The simulator's copy of crates/services/src/approval_rule.rs: which ways
// forward a pending approval has. Resolving an approval and switching a task
// to Work are admitted by `refusal` and `switchRefusal`, and the buttons the
// phone shows are `actions`, drawn from the same two, so the simulated
// computer never offers a button it would refuse. Keep it in step with the
// Rust file.

import type { ApprovalAction, ApprovalKind, Command, ProtocolError, Run, Task, TaskMode } from "../../protocol/types";

export type ApprovalDecision = Extract<Command, { command: "resolve_approval" }>["decision"];

/** What decides an approval's ways forward: what it asks for, and how its run and task work. */
export interface Asked {
  kind: ApprovalKind;
  /** The agent asks for more access (`access_request`; its kind is "other"). */
  access: boolean;
  /** The mode its run works in (`run.mode`, else the task's). */
  mode: TaskMode;
  /** The mode its run's agent was really started in. */
  launched: TaskMode;
  /** Its task is held to an access bar (a conversation, or a task carried on from one). */
  held: boolean;
  conversation: boolean;
  /** The task can carry on in Work in this session. */
  switchable: boolean;
}

/** The message a refusal for a read-only task gives. */
export const READ_ONLY = "This task only reads, so it can’t allow this. Switch it to Work to allow it.";
/** The message a refusal for a conversation gives. */
export const CHAT = "A chat can’t allow this, because it may act outside the chat’s access. Turn the chat into a task to allow it.";

const error = (code: ProtocolError["code"], message: string): ProtocolError => ({ code, message });

/** As github::writes_nothing: a GitHub finder, triage or check only reads and reports. */
export function writesNothing(task: Task): boolean {
  const type = task.github?.link.type;
  return type === "find" || type === "triage" || type === "check";
}

/** As runs::run_mode: the mode a run works in. */
export function runMode(run: Run | undefined, task: Task): TaskMode {
  return run?.mode ?? task.mode ?? "work";
}

/** Whether `task` can carry on in Work in the same session, as of a run in `mode`. */
export function switchable(task: Task, mode: TaskMode): boolean {
  const own = task.mode ?? "work";
  return task.kind === "coding" && !writesNothing(task) && !task.github?.from_stranger
    && (!!task.conversation || ((own === "plan" || own === "ask") && mode === own));
}

/** As `Asked::new`. */
export function asked(kind: ApprovalKind, mode: TaskMode, launched: TaskMode, task: Task, access = false): Asked {
  return { kind, access, mode, launched, held: task.access != null, conversation: !!task.conversation, switchable: switchable(task, mode) };
}

/** Why `decision` can't be taken, if it can't. `channel`: it comes from a chat app rather than the phone. */
export function refusal(a: Asked, decision: ApprovalDecision, channel = false): ProtocolError | null {
  const deny = decision.decision === "deny";
  const answer = decision.decision === "answer";
  if (deny) return null;
  if (a.access) {
    if (answer) return error("invalid_request", "Allow or decline this request for access.");
    if (channel) return error("permission_denied", "Allow more access on your phone, not in a chat.");
    // Held to an access bar, Allow turns on what it asked for; in Work it
    // has it all already. In a read-only task, Allow from a phone that
    // predates Switch to Work switches.
    return a.held || a.mode === "work" || a.switchable
      ? null
      : error("unsupported", "This task only reads and reports, so it can’t be given more.");
  }
  switch (a.kind) {
    // A code goes into the code prompt, which fills it.
    case "login_code":
      return error("invalid_request", "Type the code in the code prompt.");
    // A login, and a post on GitHub as the person, are allowed from the phone.
    case "login_use":
    case "github_post": {
      const what = a.kind === "login_use" ? "a login" : "a post on GitHub";
      if (answer) return error("invalid_request", `Allow or decline ${what}.`);
      if (channel) return error("permission_denied", `Allow ${what} on your phone, not in a chat.`);
      break;
    }
    default:
      break;
  }
  // A run held to its access starts as Work inside the sandbox that holds it,
  // so a command it runs or a file it edits stays within that access.
  // Anything else may act outside it.
  const contained = a.launched === "work" && (a.kind === "command" || a.kind === "file_edit");
  const answering = a.kind === "question" && answer;
  if (a.mode !== "work" && !contained && !answering) return error("unsupported", a.conversation ? CHAT : READ_ONLY);
  return null;
}

/** Why switching to Work can't take this approval along, if it can't. */
export function switchRefusal(a: Asked): ProtocolError | null {
  let wanted: boolean;
  if (a.access) wanted = !a.held && a.mode !== "work";
  else switch (a.kind) {
    case "login_code":
    case "question":
      wanted = false;
      break;
    default:
      wanted = refusal(a, { decision: "allow" }) !== null;
  }
  return wanted && a.switchable
    ? null
    : error("invalid_request", "This request doesn’t need the task switched to Work. Allow or decline it.");
}

/** The ways forward to offer on the phone, in order. */
export function actions(a: Asked): ApprovalAction[] {
  const out: ApprovalAction[] = [];
  switch (a.kind) {
    case "question":
      if (refusal(a, { decision: "answer", text: "", answers: [] }) === null) out.push("answer");
      break;
    case "login_code":
      break;
    default:
      if (switchRefusal(a) === null) out.push(a.conversation ? "turn_into_task" : "switch_to_work");
      else if (refusal(a, { decision: "allow" }) === null) out.push("allow");
  }
  out.push("deny");
  return out;
}
