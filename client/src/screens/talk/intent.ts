// SPDX-License-Identifier: Apache-2.0
//
// What the Talk composer may honestly offer for the task on screen. The
// computer decides; this only mirrors its rules (send_message needs a running
// turn, resume_task needs an ended or unstarted task) and the harness's
// reported capabilities, so the phone never offers a delivery the agent
// cannot perform. Unsupported choices stay visible with the reason.

import { defaultLaunchPolicy, launchChoices } from "../../app/launchPolicy";
import { prefs } from "../../app/prefs";
import { modelEfforts, modelName, ranAsAsked } from "./catalog";
import { branchCommand } from "../../app/taskBranch";
import type { AgentChoice } from "../../app/ui";
import { awaitingInputBack } from "../../state/handBack";
import { accessChanges } from "./access";
import { helpersFinishing, type HostState } from "../../state/projection";
import type {
  Availability,
  Command,
  ConversationAccess,
  GithubLink,
  HarnessInfo,
  MessageDelivery,
  Mission,
  Project,
  ReviewRole,
  Run,
  RunId,
  Task,
  TaskBranch,
  TaskMode,
  TaskPlace,
  TaskPlacement,
  TaskStart,
} from "../../protocol/types";

export type Intent =
  | { kind: "new_task" }
  | { kind: "message"; delivery: MessageDelivery }
  | { kind: "next_up" }
  | { kind: "continue" }
  /** Continue once the project is free: the message waits in Next up. */
  | { kind: "continue_later" };

export interface IntentOption {
  intent: Intent;
  label: string;
  detail: string;
  /** null when available; otherwise why it cannot be used now. */
  unavailable: string | null;
}

export type ComposerMode =
  | { mode: "new" }
  | { mode: "running"; options: IntentOption[] }
  | { mode: "starting" }
  | { mode: "waiting" }
  | { mode: "ended"; options: IntentOption[] }
  | { mode: "reconcile" }
  | { mode: "archived" };

const reason = (a: Availability | undefined, fallback: string): string | null => {
  if (!a) return fallback;
  switch (a.state) {
    case "available":
      return null;
    case "permission_missing":
      return a.remedy;
    case "revoked":
      return "This device’s permission for that was withdrawn on the computer";
    default:
      return a.reason;
  }
};

/**
 * The task whose run holds a project's writer. While it is held the computer
 * refuses to start a Work run there. A run that ended without a known result
 * keeps the writer until someone checks what happened, so it holds the
 * project without anything running, and the phone has to say which task that
 * is and let the person open it. A run that waits for the person holds it
 * too, and then the phone says what it waits for. So does a run whose turn
 * has ended while helpers it started are still finishing.
 */
export interface WriterHold {
  task: Task | null;
  unresolved: boolean;
  waiting: WriterWait | null;
  helpers: boolean;
}
export type WriterWait = "approval" | "input_back" | "answer";

export function writerHold(id: RunId | null | undefined, s: Pick<HostState, "runs" | "tasks" | "control">): WriterHold | null {
  if (id == null) return null;
  const run = s.runs[id];
  const task = (run && s.tasks[run.task_id]) || null;
  const unresolved = run?.state === "reconciliation_required" || task?.state === "reconciliation_required";
  const waiting = unresolved || !run || !task ? null
    : run.state === "awaiting_approval" ? "approval"
    : awaitingInputBack(task, run, s.control[task.target_id ?? ""]) ? "input_back"
    : run.state === "awaiting_input" ? "answer"
    : null;
  return { task, unresolved, waiting, helpers: !!run && helpersFinishing(run) };
}

/** How much of the holder's title a sentence names. */
export const HOLDER_TITLE = 48;
export const writerHolder = (hold: WriterHold, max = HOLDER_TITLE) => (hold.task ? `“${firstLine(hold.task.title, max)}”` : "Another task");
/** Short enough that the line beside the composer keeps its ending in two lines. */
const COMPOSER_HOLDER = 32;
const WAITS_FOR: Record<WriterWait, string> = { approval: "your approval", input_back: "you to resume it", answer: "your answer" };

/** What the holder is doing now, as a sentence without its full stop. */
export function writerActivity(hold: WriterHold, project: string): string {
  if (hold.helpers) return `Helpers from ${hold.task ? writerHolder(hold) : "another task"} are still finishing in ${project}`;
  return hold.waiting ? `${writerHolder(hold)} is waiting for ${WAITS_FOR[hold.waiting]}` : `${writerHolder(hold)} is changing files in ${project}`;
}

/** The one line a Continue that `hold` stops shows in place of the composer. */
export function writerBlockLine(hold: WriterHold, max = COMPOSER_HOLDER): string {
  const holder = hold.task ? writerHolder(hold, max) : "another task";
  if (hold.unresolved) return `Blocked until you check how ${holder} ended`;
  if (hold.helpers) return `Blocked until helpers from ${holder} finish`;
  return hold.waiting ? `Blocked: ${holder} is waiting for ${WAITS_FOR[hold.waiting]}` : `Blocked until ${holder} finishes`;
}

/** The same line when the message waits in Next up for the hold to end. */
export function writerQueueLine(hold: WriterHold): string {
  const holder = hold.task ? writerHolder(hold, COMPOSER_HOLDER) : "another task";
  if (hold.unresolved) return `Waits in Next up until you check how ${holder} ended`;
  if (hold.helpers) return `Waits in Next up until helpers from ${holder} finish`;
  return hold.waiting ? `Waits in Next up: ${holder} is waiting for ${WAITS_FOR[hold.waiting]}` : `Waits in Next up until ${holder} finishes`;
}

/** Why a Work run can't start in `project` while `hold` stands. */
export function writerHoldText(hold: WriterHold, project: string): string {
  return hold.unresolved
    ? `${writerHolder(hold)} ended without a known result, so nothing else can change files in ${project} until you check what happened.`
    : hold.waiting
      ? `${writerActivity(hold, project)}. It is changing files in ${project}, so this can’t start until it finishes.`
      : `${writerActivity(hold, project)}, so this can’t start until ${hold.helpers ? "they end" : "it finishes"}.`;
}

/** The same hold, for a read-only run that may start beside it. */
export function writerHoldBesideText(hold: WriterHold, project: string, mode: string): string {
  return hold.unresolved
    ? `${writerHolder(hold)} ended without a known result and may have left files in ${project} half changed. ${mode} only reads, so it can start now.`
    : hold.waiting
      ? `${writerActivity(hold, project)} partway through changing files in ${project}. ${mode} only reads, so it can start now, but it may see those changes half done.`
      : `${writerActivity(hold, project)}. ${mode} only reads, so it can start now, but it may see those changes half done.`;
}

/** What a queued Work task at the front of Next up waits for while `hold` stands. */
export function writerHoldState(hold: WriterHold): string {
  const holder = hold.task ? writerHolder(hold) : "another task";
  if (hold.unresolved) return `Waiting for you to check ${holder}`;
  if (hold.helpers) return `After helpers from ${holder} finish`;
  switch (hold.waiting) {
    case "approval": return `Waiting for your approval on ${holder}`;
    case "input_back": return `Waiting for you to resume ${holder}`;
    case "answer": return `Waiting for your answer to ${holder}`;
    default: return `After ${holder} finishes`;
  }
}

/**
 * @param writer the run holding this project's writer, if any. The computer
 *   refuses to start a Work run here until it is released. Ask and Plan runs
 *   only read, so they start beside it.
 */
export function composerMode(task: Task | null, run: Run | null, harness: HarnessInfo | null, writer: WriterHold | null = null, project = "this project"): ComposerMode {
  if (!task) return { mode: "new" };
  const name = harness?.display_name ?? "The agent";
  // A new task from one that only reads starts in Work (Review), but a
  // stranger's work in Next up stays read-only.
  const reads = (task.mode ?? "work") !== "work" ? task.mode === "plan" ? "Plan" : "Ask" : null;
  switch (task.state) {
    case "working":
    case "awaiting_decision": {
      const waiting = run?.state === "awaiting_approval" || run?.state === "awaiting_input";
      const steer = waiting
        ? "Answer the waiting request first, or send after this turn"
        : reason(harness?.capabilities.steer_running, `${name} doesn’t report whether it can take messages mid-turn`);
      const interrupt = reason(harness?.capabilities.interrupt, `${name} doesn’t report whether it can be interrupted`);
      const options: IntentOption[] = [
        { intent: { kind: "message", delivery: "steer" }, label: "Steer now", detail: `${name} reads this while it works.`, unavailable: steer },
        { intent: { kind: "message", delivery: "next_turn" }, label: "After this turn", detail: "Held until the current turn ends, then delivered.", unavailable: null },
        {
          intent: { kind: "message", delivery: "interrupt_and_send" },
          label: "Stop the turn and send",
          detail: "Ends what it’s doing now, then sends this.",
          unavailable: interrupt,
        },
        { intent: { kind: "next_up" }, label: "Add to Next up", detail: reads && !task.github?.from_stranger ? `Saved as a new task that waits its turn, in Work mode so it can change files. This task stays in ${reads}.` : "Saved as a new task that waits its turn. This task isn’t changed.", unavailable: null },
      ];
      // A conversation's messages stay in it; work for later is a task in Work.
      return { mode: "running", options: task.conversation ? options.filter((o) => o.intent.kind !== "next_up") : options };
    }
    case "accepted":
      return { mode: "waiting" };
    case "dispatching":
      return { mode: "starting" };
    case "completed":
    case "failed":
    case "interrupted": {
      const start = writer && (task.mode ?? "work") === "work"
        ? writer.unresolved
          ? `Can’t continue yet: ${writerHoldText(writer, project)} Check it first, or put your message in Next up.`
          : writer.helpers
            ? `Can’t continue yet: ${writerActivity(writer, project)}. Continue once they end, or stop them.`
            : `Can’t continue yet: ${writerActivity(writer, project)}. Put your message in Next up to continue when it finishes.`
        : reason(harness?.capabilities.start_run, `${name} isn’t available on this computer`)
          ?? reason(harness?.capabilities.resume_session, `${name} doesn’t report that it can continue a task`);
      const fresh = harness?.capabilities.resume_session.state === "available" ? "" : ` ${name} starts a fresh session and won’t remember this conversation.`;
      if (task.conversation)
        return { mode: "ended", options: [{ intent: { kind: "continue" }, label: "Reply", detail: `Goes on with the conversation. ${task.access && accessChanges(task.access) ? `${name} may change files where you’ve let it, and run commands there.` : `${name} reads but changes nothing.`}${fresh}`, unavailable: start }] };
      // Held only by the project's writer, the message can wait its turn.
      const later: IntentOption[] = writer && (task.mode ?? "work") === "work" && !reason(harness?.capabilities.start_run, "unknown")
        ? [{ intent: { kind: "continue_later" }, label: "Continue in Next up", detail: `Waits in Next up, then continues this task with your message. ${writerHoldState(writer)}.${fresh}`, unavailable: null }]
        : [];
      return {
        mode: "ended",
        options: [
          { intent: { kind: "continue" }, label: "Continue this task", detail: `Starts a new run of this task with your message.${fresh}`, unavailable: start },
          ...later,
          { intent: { kind: "new_task" }, label: "Start a new task", detail: reads ? `A separate task in this project, in Work mode so it can change files. This one stays in ${reads}.` : "A separate task in this project. This one stays as it is.", unavailable: null },
        ],
      };
    }
    case "reconciliation_required":
      return { mode: "reconcile" };
    case "archived":
      return { mode: "archived" };
  }
}

/** The option the composer starts on: the least disruptive one that works. */
export function defaultIntent(mode: ComposerMode): Intent {
  if (mode.mode === "ended") {
    const later = mode.options.find((o) => o.intent.kind === "continue_later");
    return mode.options[0]?.unavailable && later ? later.intent : { kind: "continue" };
  }
  if (mode.mode === "running") {
    const first = mode.options.find((o) => o.unavailable === null);
    if (first) return first.intent;
  }
  return { kind: "new_task" };
}

export const sameIntent = (a: Intent, b: Intent): boolean =>
  a.kind === b.kind && (a.kind !== "message" || (b.kind === "message" && a.delivery === b.delivery));

/**
 * Whether the send must pass through Request review. Anything that
 * creates a task, or carries marked-up material, is reviewed; a plain message
 * to the task on screen is sent from Talk. Markup waiting in Next up is
 * reviewed like markup sent at once.
 */
export function needsReview(intent: Intent, hasAttachment: boolean): boolean {
  if (intent.kind === "new_task" || intent.kind === "next_up") return true;
  return hasAttachment;
}

/** The button text for the composer's send action. */
export function sendLabel(intent: Intent, hasAttachment: boolean, conversation = false): string {
  if (needsReview(intent, hasAttachment)) return "Review";
  if (intent.kind === "continue") return conversation ? "Send" : "Continue";
  if (intent.kind === "continue_later") return "Put in Next up";
  return "Send";
}

// ---- Agent choice ---------------------------------------------------------

const EFFORT: Record<string, string> = {
  minimal: "Minimal",
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Max",
};

export const effortLabel = (e: string): string => EFFORT[e] ?? e.charAt(0).toUpperCase() + e.slice(1);

/** A harness can start managed work from the phone. */
export const canStart = (h: HarnessInfo): boolean => h.integration === "managed" && h.capabilities.start_run.state === "available";

/** The agent Settings names for new work, when this computer has it ready,
 * else the first that is. */
export function defaultAgent(harnesses: HarnessInfo[]): HarnessInfo | undefined {
  const named = prefs.get().defaultAgent;
  return harnesses.find((h) => h.kind === named && canStart(h)) ?? harnesses.find(canStart);
}

/** A saved choice stays visible even when unavailable. Only a task with no
 * choice or project default may be offered the default agent from Settings,
 * or the first ready one. */
export function resolveAgent(project: Project | undefined, harnesses: HarnessInfo[], chosen?: AgentChoice | null): AgentChoice | null {
  if (chosen) return chosen.launchPolicy !== undefined ? chosen : { ...chosen, launchPolicy: defaultLaunchPolicy(chosen.harness) };
  if (project?.default_harness) return { harness: project.default_harness, model: project.default_model, effort: project.default_effort, launchPolicy: defaultLaunchPolicy(project.default_harness) };
  const h = defaultAgent(harnesses);
  return h ? { harness: h.kind, model: null, effort: null, launchPolicy: defaultLaunchPolicy(h.kind) } : null;
}

/** Validate without changing who receives the request. */
export function agentChoiceBlocker(h: HarnessInfo | undefined, choice: AgentChoice): string | null {
  if (choice.launchPolicy && !launchChoices(choice.harness).some(c => c.value === choice.launchPolicy)) return "That permission setting is unavailable for the selected agent. Choose one before starting.";
  if (!h) return "The selected agent is no longer reported by this computer. Choose an agent before starting.";
  if (!canStart(h)) return reason(h.capabilities.start_run, `${h.display_name} can’t start tasks.`) ?? `${h.display_name} can’t start managed tasks.`;
  const installed = reason(h.installed, `${h.display_name} isn’t installed.`);
  if (installed) return installed;
  if (h.account.state !== "signed_in") return h.account.state === "signed_out" ? h.account.remedy : h.account.reason;
  if (choice.model && (h.capabilities.model_selection.state !== "available" || !h.models.some(m => m.id === choice.model)))
    return `The selected model (${choice.model}) is unavailable. Choose a model; it won’t be replaced with the agent’s default.`;
  if (choice.effort && (h.capabilities.effort_selection.state !== "available" || !(h.models.some(m => m.details) ? modelEfforts(h, choice.model).some(e => e.id === choice.effort) : h.efforts.includes(choice.effort))))
    return `The selected effort (${choice.effort}) is unavailable for this model. Choose an effort before starting.`;
  return null;
}

/** "Claude Code · Opus 5.5 · High", naming defaults as such. */
export function agentLabel(harnesses: HarnessInfo[], a: AgentChoice): string {
  const h = harnesses.find((x) => x.kind === a.harness);
  const agent = h?.display_name ?? a.harness;
  const parts = [agent];
  const model = a.model ?? h?.models.find((m) => m.is_default)?.id;
  if (model) parts.push(modelName(model, h, agent));
  if (a.effort) parts.push(effortLabel(a.effort));
  return parts.join(" · ");
}

export interface RunAgent {
  label: string;
  /** Set when the agent reported something other than what was asked. */
  differs: string | null;
  /** True until the agent reports what it actually runs. */
  requestedOnly: boolean;
}

/** What actually ran, from the native harness's own report. */
export function runAgent(harnesses: HarnessInfo[], run: Run): RunAgent {
  const h = harnesses.find((x) => x.kind === run.harness);
  const model = run.actual_model ?? run.requested_model;
  const effort = run.actual_effort ?? run.requested_effort;
  const agent = h?.display_name ?? run.harness;
  const parts = [agent];
  if (model) parts.push(modelName(model, h, agent));
  if (effort) parts.push(effortLabel(effort));
  const asked: string[] = [];
  if (run.actual_model && run.requested_model && !ranAsAsked(run.requested_model, run.actual_model)) asked.push(modelName(run.requested_model, h));
  if (run.actual_effort && run.requested_effort && run.actual_effort !== run.requested_effort) asked.push(effortLabel(run.requested_effort));
  return {
    label: parts.join(" · "),
    differs: asked.length ? `You asked for ${asked.join(", ")}` : null,
    requestedOnly: run.actual_model === null && run.acknowledged_at === null,
  };
}

// ---- Commands -------------------------------------------------------------

export interface NewTaskInput {
  projectId: string;
  prompt: string;
  title: string | null;
  agent: AgentChoice;
  mode: TaskMode;
  placement: TaskPlacement;
  mission: Mission | null;
  bundle: Extract<Command, { command: "create_task" }>["bundle"];
  /** The Git branch the agent is told to work on or make; null leaves it to the agent. */
  branch?: TaskBranch | null;
  /** The agent, skill or scratch pad it starts from. */
  start?: TaskStart | null;
  /** A task on its own, in a new folder; `projectId` is then the new project's. */
  place?: TaskPlace | null;
  /** Whether Work runs may use the agent browser and logins; null leaves the computer's default. */
  browser?: boolean | null;
  /** What GitHub work it is for; null is none. */
  github?: GithubLink | null;
  /** A conversation for Talk rather than a task. */
  conversation?: boolean;
  /** What a conversation may reach, on a computer that holds it to that. */
  access?: ConversationAccess | null;
  /** Its part in a review, which gives it the project's review rules; only for a computer that has them. */
  review?: ReviewRole | null;
}

export function createTaskCommand(i: NewTaskInput): Command {
  return {
    command: "create_task",
    agent_chosen: true,
    project_id: i.projectId,
    kind: "coding",
    mode: i.mode,
    title: i.title?.trim() || null,
    prompt: i.prompt.trim(),
    bundle: i.bundle,
    harness: i.agent.harness,
    model: i.agent.model,
    effort: i.agent.effort,
    ...(i.agent.launchPolicy != null ? { launch_policy: i.agent.launchPolicy } : {}),
    ...(i.branch ? { branch: branchCommand(i.branch) } : {}),
    ...(i.start ? { start: i.start } : {}),
    ...(i.place ? { place: i.place } : {}),
    ...(i.browser != null ? { browser: i.browser } : {}),
    ...(i.github ? { github: i.github } : {}),
    ...(i.conversation ? { conversation: true } : {}),
    ...(i.access ? { access: i.access } : {}),
    ...(i.review ? { review: i.review } : {}),
    mission: i.mission,
    target_id: null,
    placement: i.placement,
  };
}

/**
 * A plain-language stopping point becomes a mission whose only bound is the
 * person's request. It is never marked enforced: adapters cannot enforce it.
 */
export function missionFrom(goal: string, stopPoint: string, boundaries = "", checks = ""): Mission | null {
  const stop = stopPoint.trim();
  const lines = (text: string) => text.split("\n").map((line) => line.trim()).filter(Boolean);
  const bounds = lines(boundaries).map((text) => ({ text, enforcement: "requested_only" as const }));
  const completion_checks = lines(checks);
  if (!stop && !bounds.length && !completion_checks.length) return null;
  return { goal: goal.trim().split("\n")[0]!.slice(0, 200), bounds, completion_checks, stop_point: stop || null };
}

/** One readable line for a prompt, used where the host has not titled a task yet. */
export function firstLine(text: string, max = 80): string {
  const line = text.trim().split("\n")[0] ?? "";
  if (line.length <= max) return line;
  // Cut at a word, unless that would leave too little of it.
  const cut = line.slice(0, max - 1), space = cut.lastIndexOf(" ");
  return `${(space > max / 2 ? cut.slice(0, space) : cut).trimEnd()}…`;
}

/**
 * What the harness declared for a task mode. A harness that declares no modes
 * is Work-only: nothing it reports restricts the agent to reading.
 */
export function modeSupport(h: HarnessInfo | undefined, mode: TaskMode): { availability: Availability; enforcement: string | null } {
  const declared = h?.modes?.find((m) => m.mode === mode);
  if (declared) return { availability: declared.availability, enforcement: declared.enforcement };
  if (mode === "work") return { availability: { state: "available" }, enforcement: null };
  return {
    availability: { state: "unsupported", reason: `${h?.display_name ?? "This agent"} doesn’t report a way to keep it read-only.` },
    enforcement: null,
  };
}
