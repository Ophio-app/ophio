// SPDX-License-Identifier: Apache-2.0
import { ReproductionReview } from "../annotate/ReproductionReview";
//
// Request review: the last look before the computer is asked to do
// something. It shows exactly what will be sent, to which folder and agent,
// what the agent may do without asking, and when it will start. Nothing
// here is decorative; every line is something the person could want to
// change or would be surprised by later.

import { Bot, ChevronDown, ChevronRight, FolderGit2, FolderPlus, GitBranch, GitBranchPlus, GitPullRequest, Monitor, NotebookPen, Paperclip, Sparkles, TriangleAlert, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { describeError, shallowEqual, useApp, useCommand, useHost, useUi, useLink } from "../../app/context";
import { draftKey, freshReview, REVIEW_FLOW, type AgentChoice, type ReviewDetails, type ReviewIntent, type SendAttempt } from "../../app/ui";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { Chip, Segmented } from "../../design/Chip";
import { TextArea, TextField } from "../../design/Field";
import { Empty, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { toast } from "../../design/Toast";
import type { Command, GithubLink, HarnessInfo, MessageDelivery, QueueAuthorization, Task, TaskBranch, TaskMode, TaskPlace, TaskPlacement } from "../../protocol/types";
import { uuidv7 } from "../../protocol/ids";
import { LAUNCHERS_FEATURE, LAUNCHERS_UPDATE, ONE_OFF_DRAFT, ONE_OFF_PROJECT, localDate, inheritedStart, projectSource, slug, startKind, startedLabel, useCatalog, type LaunchStart } from "../../app/launchers";
import { annotationStore, commentUnresolved, describe, summarize, useAnnotation, type AnnotationDraft } from "../../state/annotations";
import { Screen, useRoute } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { TaskRecipient, useRetryRecipient, useTaskRecipient } from "./TaskRecipient";
import { launchRisk, launchChoices, FIX_LAUNCH_FEATURE, FIX_LAUNCH_UPDATE, LAUNCH_POLICY_FEATURE, LAUNCH_POLICY_UPDATE } from "../../app/launchPolicy";
import { branchStatus, TASK_BRANCH_FEATURE, TASK_BRANCH_UPDATE } from "../../app/taskBranch";
import { BranchSheet, useBranches } from "./BranchPicker";
import { prefs } from "../../app/prefs";
import { useStore } from "../../state/store";
import { AgentSheet } from "./AgentSheet";
import { StartFromSheet } from "../launch/StartFrom";
import { AttemptNotice } from "./Composer";
import { removeMarkup } from "./removedMarkup";
import { agentChoiceBlocker, agentLabel, canStart, composerMode, createTaskCommand, firstLine, missionFrom, modeSupport, resolveAgent, writerHold, writerHoldBesideText, writerHoldText } from "./intent";
import { AGENT_LOGINS_FEATURE } from "../../app/agentLoginsFeature";
import { QUEUE_MARKUP_FEATURE, QUEUE_MARKUP_UPDATE } from "../../app/queueMarkupFeature";
import { PRODUCT_NAME } from "../../brand";
import { ModePicker, modeLabel } from "./ModePicker";
import { accessHeld, describeAccess, heldAccess } from "./access";
import { ModeSwitch } from "./ModeSwitch";
import { LaunchLine } from "./LaunchLine";
import { check, repeatable, resend, send, useAutoSettle, type SendRequest } from "./send";
import { sendAsked, takeSendAsk } from "./sendOnOpen";
import { QueueReview } from "./QueueReview";
import { startSummary } from "./startSummary";
import { ReadinessCheck } from "../work/ReadinessCheck";
import { useOpenTask } from "../work/shared";
import { taskBadge } from "./TaskBar";
import { continuationBlocker } from "../control/readiness";
import { Unlock, useUnlockable } from "../control/Unlock";
import { holdsHumanInput } from "../control/controlStatus";
import { handBackTarget } from "../../state/handBack";
import { returnToAgent } from "./returnToAgent";
import { latestRun } from "../work/logic";
import { PathText } from "../../design/PathText";
import { Markdown } from "../../design/Markdown";
import { useCarryPlan, withPlan } from "./carryPlan";
import { choiceOf, fixChoiceOf, fixSent, lastRole, linkDetail, linkWords, planOnlyText, planOnlyWhy, readOnlyLink, reminderText, rememberRole, useGithubProject, useGithubSupported } from "../../app/github";
import { GITHUB_UPDATE } from "../../app/githubFeature";
import { LinkSheet } from "../github/LinkSheet";
import { ThroughMergeWarning, answerWarning, useGithubRun } from "../github/common";

const TITLE: Record<ReviewIntent, string> = {
  new_task: "Review new task",
  carry_out_plan: "Review Work from this plan",
  next_up: "Review Next up task",
  continue: "Continue task",
  steer: "Review message",
  next_turn: "Review message",
  interrupt_and_send: "Review message",
};

const DELIVERY: Record<MessageDelivery, { label: string; detail: string }> = {
  steer: { label: "Steer now", detail: "Read while it works." },
  next_turn: { label: "After this turn", detail: "Held until the current turn ends." },
  interrupt_and_send: { label: "Stop the turn and send", detail: "Ends what it’s doing now, then sends this." },
};

/** What the primary action says once pressed. It stays until the answer:
 * the request itself can change what the label was chosen from, as a started
 * task taking the project's writer turns Start task into Add to Next up. */
const UNDERWAY: Record<string, string> = {
  "Start task": "Starting…",
  "Add to Next up": "Adding to Next up…",
  "Start Work": "Starting Work…",
  Continue: "Continuing…",
  "Stop and send": "Stopping and sending…",
  Send: "Sending…",
  "Send and resume": "Sending…",
};

/** What Work may do without asking; a warning when that's everything. */
function Permission({ risky, children }: { risky: boolean; children: string }) {
  return <p className={risky ? "review__permission launch-line__risk" : "review__permission"} role={risky ? "note" : undefined}>
    {risky && <TriangleAlert aria-hidden strokeWidth={2.2} />}
    <span>{children}</span>
  </p>;
}

/** What the agent may do on the computer without asking, stated plainly. */
function permissionText(h: HarnessInfo | undefined, computer: string, phoneApproves: boolean, policy?: import("../../protocol/types").LaunchPolicy | null): string {
  if (policy && h) return launchChoices(h.kind).find(c => c.value === policy)?.detail ?? "Unsupported permission setting.";
  const where = phoneApproves ? "this device" : computer;
  switch (h?.kind) {
    case "claude_code":
      return `Asks on ${where} before editing files or running commands, except what Claude Code’s own settings on ${computer} already allow.`;
    case "codex":
      return `Works within the sandbox set in Codex’s settings on ${computer}, and asks on ${where} when it needs more.`;
    default:
      return h?.capabilities.native_approvals.state === "available"
        ? `Asks on ${where} when ${h.display_name} decides it needs approval, following its settings on ${computer}.`
        : `Follows ${h?.display_name ?? "the agent"}’s own settings on ${computer}. It doesn’t send approval requests to the device.`;
  }
}

const sentence = (s: string | null) => (s ? (/[.!?]$/.test(s) ? s : `${s}.`) : "");

/** How the agent is given what the task starts from (`ports::harness::start_instruction`).
 * The computer may have the agent load its own agents and skills by name
 * instead, which nothing here can promise, so neither is said. */
function startNote(start: LaunchStart, h: HarnessInfo | undefined): string {
  const note = {
    scratch: "Nothing is added to what you write.",
    agent: `Its instructions go to ${h?.display_name ?? "the agent"} with your request.`,
    skill: `${h?.display_name ?? "The agent"} follows its instructions for this task.`,
  }[startKind(start)];
  return start.start.type === "same_as" ? `Kept from the task it follows, as that task was given it. ${note}` : note;
}

export default function Review({ route }: RouteProps<"review">) {
  return route.queueItemId ? <QueueReview route={route} itemId={route.queueItemId} /> : <RequestReview route={route} />;
}

/** What the markup's comments say and which marks each is about, as the
 * agent reads them. Only shown: the Markup row above opens the editor. */
function MarkupComments({ annotation }: { annotation: AnnotationDraft }) {
  const comments = annotation.comments.filter(comment => comment.text.trim());
  if (!comments.length) return null;
  const labels = (ids: string[]) => ids.map(id => annotation.marks.find(mark => mark.mark_id === id)?.label ?? "Missing mark").join(", ");
  return (
    <ul className="review__markup" aria-label="Markup comments">
      {comments.map(comment => (
        <li key={comment.comment_id}>
          <span className="review__markup-to">{commentUnresolved(annotation, comment)
            ? `Unresolved · ${labels(comment.mark_ids) || "no mark"}`
            : labels(comment.mark_ids) || "General"}</span>{" "}
          {comment.text}
        </li>
      ))}
    </ul>
  );
}

export function annotationReviewIntent(task: Task | undefined): ReviewIntent {
  return task && ["completed", "failed", "interrupted"].includes(task.state) ? "continue" : "next_turn";
}

/** Sends what a button already said to send once nothing here stands in its
 * way, at most once. A touch or key first leaves it to the person. */
function SendWhenClear({ clear, onSend, setGoing }: { clear: boolean; onSend: () => void; setGoing: (going: boolean) => void }) {
  const sent = useRef(false);
  useEffect(() => {
    if (!clear || sent.current) return;
    sent.current = true;
    onSend();
  }, [clear, onSend]);
  useEffect(() => {
    const leave = () => { if (!sent.current) setGoing(false); };
    document.addEventListener("pointerdown", leave, true);
    document.addEventListener("keydown", leave, true);
    return () => {
      document.removeEventListener("pointerdown", leave, true);
      document.removeEventListener("keydown", leave, true);
    };
  }, [setGoing]);
  return null;
}

function RequestReview({ route }: RouteProps<"review">) {
  const app = useApp();
  const { ui, profile } = app;
  const { tab: originTab } = useRoute();
  const stillHere = () => ui.get().tab === originTab && ui.route() === route;
  const close = <Button onClick={() => ui.finish(REVIEW_FLOW, originTab, route)}>Close review</Button>;
  const reportFailure = (detail: string) => {
    if (stillHere()) announce(detail, "assertive");
    else toast(`${detail} Your draft is kept in Review.`, { tone: "stop", action: { label: "Review", run: () => ui.push(route, originTab) } });
  };
  const online = useLink().state === "online";
  const { projectId, taskId } = route;
  const selectedProjectId = useUi(s => s.projectId);
  const hostProject = useHost((s) => s.projects[projectId]);
  const projects = useHost((s) => s.projects);
  const accessIsHeld = useHost((s) => accessHeld(s.capabilities));
  // A draft made with no project chosen has only the folder it will get.
  const project = hostProject ?? (projectId === ONE_OFF_DRAFT ? ONE_OFF_PROJECT : undefined);
  const task = useHost((s) => (taskId ? s.tasks[taskId] : undefined));
  const intent: ReviewIntent = route.intent ?? (taskId ? annotationReviewIntent(task) : "new_task");
  const carryingPlan = intent === "carry_out_plan";
  const continues = intent === "continue" || carryingPlan;
  const controlReason = useHost((s) => continues ? continuationBlocker(task, s.capabilities, s.targets, s.control, s.grants) : null);
  const unlockable = useUnlockable(continues ? task?.target_id : null);
  const run = useHost((s) => task ? carryingPlan ? latestRun(task, s.runs) : s.runs[task.current_run ?? ""] : undefined);
  // The plan on the route was the latest when Review opened; a later Plan run replaces it.
  const latestPlan = useCarryPlan(carryingPlan ? task : null);
  const [reviewedTask, setReviewedTask] = useState(task);
  const [reviewedRun, setReviewedRun] = useState(run);
  const taskChanged = continues && reviewedTask?.revision !== task?.revision;
  const harnesses = useHost((s) => s.harnesses);
  const grants = useHost((s) => s.grants);
  const queue = useHost((s) => s.queues[projectId]);
  const draft = useUi((s) => s.drafts[draftKey(projectId, taskId)] ?? null);
  // A new task on its own gets a new folder, so nothing of the draft's
  // project (its writer, branches or Next up) applies to it.
  const alone = intent === "new_task" && taskId === null && (projectId === ONE_OFF_DRAFT || draft?.review?.place === "one_off");
  const launchersSupported = useHost((s) => !!s.capabilities?.features.includes(LAUNCHERS_FEATURE));
  const browserSupported = useHost((s) => !!s.capabilities?.features.includes(AGENT_LOGINS_FEATURE));
  const queueMarkup = useHost((s) => !!s.capabilities?.features.includes(QUEUE_MARKUP_FEATURE));
  const store = annotationStore(profile.profileId);
  const annotation = useAnnotation(store, draft?.annotationId ?? null);

  const requestField = useRef<HTMLTextAreaElement>(null);
  const prefilled = useRef(false);
  const [fromComment, setFromComment] = useState(false);
  const [requestError, setRequestError] = useState<string | null>(null);
  useEffect(() => {
    if (requestError) requestField.current?.scrollIntoView?.({ block: "nearest" });
  }, [requestError]);
  const creates = intent === "new_task" || intent === "next_up";
  // Opened from a hand-back's Mark up: while that run still waits for input
  // back on its target, sending the drawing gives the input back as well.
  const execute = useCommand();
  const backView = useHost((s) => route.handBack ? s.control[route.handBack.targetId] : undefined);
  const backTarget = useHost((s) => route.handBack ? s.targets.find((t) => t.target_id === route.handBack?.targetId) : undefined);
  const waitingBack = useHost((s) => task ? handBackTarget(s, task) : undefined);
  const backMine = holdsHumanInput(backView, profile.deviceId);
  const handingBack = route.handBack && backTarget && !creates && !continues && task?.current_run === route.handBack.runId
    && task.target_id === route.handBack.targetId && (backMine || waitingBack?.target_id === route.handBack.targetId) ? route.handBack : null;
  const taskRecipient = useTaskRecipient(project, creates ? null : task);
  const retryRecipient = useRetryRecipient(projectId, taskId);
  // A message to a task needs no text of its own: the agent reads the
  // markup's comments, so they are named here rather than copied in.
  const commented = annotation?.comments.filter(comment => comment.text.trim()).length ?? 0;
  useEffect(() => {
    if (!creates || !annotation || prefilled.current) return;
    const comment = annotation.comments.find(comment => comment.text.trim())?.text;
    if (!comment) return;
    prefilled.current = true;
    if (!draft?.text.trim()) {
      ui.saveDraft(projectId, taskId, { text: comment });
      setFromComment(true);
    }
  }, [annotation, creates, draft?.text, projectId, taskId, ui]);
  const projectWriter = useHost((s) => writerHold(s.projects[projectId]?.active_writer, s), shallowEqual);
  const writer = alone ? null : projectWriter;
  const openTask = useOpenTask();
  // A Next up item from a running task defaults to that task's agent, and to
  // its launch flags, or the person's own when it has none: a new task is a
  // new launch.
  const base: AgentChoice | null = draft?.agent ?? (task && intent === "next_up" ? { harness: task.harness, model: task.model, effort: task.effort, launchPolicy: task.launch_policy ?? undefined } : null);
  useStore(prefs, p => p);
  const launchSupported = useHost(s => !!s.capabilities?.features.includes(LAUNCH_POLICY_FEATURE));
  const fixLaunchSupported = useHost(s => !!s.capabilities?.features.includes(FIX_LAUNCH_FEATURE));
  const agent = creates ? resolveAgent(project, harnesses, base) : null;
  const harness = harnesses.find((h) => h.kind === (creates ? agent?.harness : task?.harness));
  // Kept locally as well as on the draft, so clearing the text never resets
  // the mode. A new task starts in Work, even from a task that only reads:
  // what a Plan finds is usually carried out next. A stranger's work stays
  // read-only, as the computer would keep it. New work an agent proposed
  // keeps its task's mode on the draft.
  const fromMode: TaskMode | null = !creates ? null : task ? (task.mode ?? "work") : (draft?.review?.from ?? null);
  const [mode, setModeState] = useState<TaskMode>(draft?.mode ?? (task?.github?.from_stranger && intent === "next_up" ? fromMode ?? "work" : "work"));
  const setMode = (m: TaskMode) => {
    setModeState(m);
    ui.saveDraft(projectId, taskId, { mode: m });
  };
  const support = modeSupport(harness, mode);

  // Only Work takes the project's writer; Ask and Plan read beside it.
  const writerBlocks = writer !== null && mode === "work";
  // A start-now choice the computer would refuse shows as Next up, and comes
  // back if the mode changes to one that can start beside the writer.
  // Title, stopping point and placement live on the draft, so going Back to
  // adjust the request, or a restart, never loses them.
  const details: ReviewDetails = draft?.review ?? freshReview();
  const setDetails = (patch: Partial<ReviewDetails>) => ui.saveDraft(projectId, taskId, { review: { ...details, ...(ui.draft(projectId, taskId)?.review ?? {}), ...patch } });
  const placement = alone ? "start_now" : writerBlocks ? "queue" : (details.placement ?? (intent === "next_up" ? "queue" : "start_now"));
  // A Next up item starts as its source task did until another start is
  // chosen or this one is removed (null).
  const start = !creates ? null : details.start !== undefined ? details.start : intent === "next_up" && task ? inheritedStart(task) : null;
  const setPlacement = (p: "start_now" | "queue") => setDetails({ placement: p });
  const { authorization, title, stopWhen: stop } = details;
  const setAuthorization = (a: QueueAuthorization) => setDetails({ authorization: a });
  const setTitle = (t: string) => setDetails({ title: t });
  const setStop = (t: string) => setDetails({ stopWhen: t });
  // Optional stays folded unless something in it is already filled in.
  const [optionalOpen] = useState(() => [title, details.bounds, details.completionChecks, stop].some((v) => !!v?.trim()));
  const [choosing, setChoosing] = useState(false);
  const [choosingStart, setChoosingStart] = useState(false);
  // A new task names no branch unless chosen; a Next up item starts with its
  // source task's, which that task will already have made if it was new.
  // Other requests show the task's own, which can't change.
  const chosenBranch: TaskBranch | null = alone ? null : creates
    ? details.branch !== undefined ? details.branch : intent === "next_up" && task?.branch ? { name: task.branch.name } : null
    : task?.branch ?? null;
  const capabilitiesKnown = useHost((s) => s.capabilities !== null && s.capabilities !== undefined);
  const branchSupported = useHost((s) => !!s.capabilities?.features.includes(TASK_BRANCH_FEATURE));
  const branches = useBranches(projectId, branchSupported && online && !!grants?.tasks && !alone);
  // Where a task on its own will be, as the computer reads it now.
  const oneOffs = useCatalog(null, alone && launchersSupported && online && !!grants?.tasks);
  const [choosingBranch, setChoosingBranch] = useState(false);
  // What GitHub work it's for: unless chosen, the project's GitHub setting
  // links Work to a new pull request, and a Next up item to its source's.
  const githubSupported = useGithubSupported();
  const githubAsked = creates && !alone && githubSupported && !!grants?.tasks && project?.vcs?.kind === "git";
  const githubProject = useGithubProject(githubAsked ? projectId : null);
  const githubView = githubAsked ? githubProject.view : null;
  const githubOn = !!githubView && githubView.settings.mode !== "off";
  const sourcePr = intent === "next_up" && task?.github?.pr?.state === "open" ? task.github.pr.number : null;
  const defaultLink: GithubLink | null = !githubOn ? null : sourcePr !== null ? { type: "pr", number: sourcePr } : mode === "work" ? { type: "new_pr" } : null;
  const link: GithubLink | null = !creates || alone ? null : details.github !== undefined ? details.github : defaultLink;
  // A Find's own limit and what follows it, else the project's.
  const findThen = link?.type === "find" ? link.fix_findings ?? githubView?.settings.workflow.fix_findings ?? null : null;
  const findMost = link?.type === "find" ? link.max_issues ?? githubView?.settings.workflow.findings_per_run ?? null : null;
  // A pull request taken over is worked on where it is.
  const sentBranch = link?.type === "pr" ? null : chosenBranch;
  const fixDefault = lastRole(profile.profileId, projectId, "fix") ?? (agent ? choiceOf(agent) : null);
  const [choosingGithub, setChoosingGithub] = useState(false);
  const [choosingFix, setChoosingFix] = useState(false);
  const warning = useGithubRun();
  const [sending, setSending] = useState<{ label: string; detail: string | null } | null>(null);
  const [checking, setChecking] = useState(false);
  const [done, setDone] = useState(false);
  // A button that already said what to send, such as Start on work an agent
  // found, is sent from here at once unless something needs choosing first.
  const [going, setGoing] = useState(() => sendAsked(projectId, taskId));
  useEffect(() => { takeSendAsk(projectId, taskId); }, [projectId, taskId]);

  useAutoSettle(projectId, taskId, draft?.attempt ?? null);

  if (!project || (taskId && !task)) {
    return (
      <Screen title={TITLE[intent]}>
        <Empty title="This isn’t on the computer any more" action={close}>
          The {taskId ? "task" : "project"} may have been removed on {profile.name}. What you wrote is still in Chat.
        </Empty>
      </Screen>
    );
  }
  if (!draft && !done) {
    return (
      <Screen title={TITLE[intent]}>
        <Empty title="Nothing to review" action={close}>
          The message was sent or cleared.
        </Empty>
      </Screen>
    );
  }

  const text = draft?.text ?? "";
  const attempt = draft?.attempt ?? null;
  const setText = (v: string) => { setRequestError(null); ui.saveDraft(projectId, taskId, { text: v }); };

  // Whether the message can still go the way it was chosen in Talk.
  const composer = task ? composerMode(task, run ?? null, harness ?? null, writer, project?.name) : null;
  const optionFor = (kind: "continue" | MessageDelivery) =>
    composer && (composer.mode === "running" || composer.mode === "ended")
      ? composer.options.find((o) => (kind === "continue" ? o.intent.kind === "continue" : o.intent.kind === "message" && o.intent.delivery === kind))
      : undefined;
  // Held by the project's writer, a continuation waits in Next up instead.
  const later = intent === "continue" && optionFor("continue")?.unavailable && composer?.mode === "ended"
    ? composer.options.find((o) => o.intent.kind === "continue_later") : undefined;
  const chosenOption = !creates ? (later ?? optionFor((carryingPlan ? "continue" : intent) as "continue" | MessageDelivery)) : undefined;
  const stale = !creates && (!chosenOption || chosenOption.unavailable !== null);

  // While the last send's fate is unknown, the primary action repeats that
  // request under its id; a new request needs an answer first, or Send anyway.
  const unresolved = attempt?.phase === "unknown" ? attempt : null;
  const launchBlocker = !creates || (!start && !alone) ? null
    : !launchersSupported ? LAUNCHERS_UPDATE
    : alone && start && projectSource(start.source) ? `“${start.name}” is kept in ${hostProject?.name ?? "a project"}’s own folder, so it can start only there.`
    : null;
  // The fixes' launch flags are their own, not the finder's: it reads in Plan.
  const fixWith = link?.type === "find" ? fixSent(link.fix_with, fixLaunchSupported) : null;
  const fixBlocker = fixWith && findThen !== "file_only" ? agentChoiceBlocker(harnesses.find((h) => h.kind === fixWith.harness), fixWith) : null;
  const githubBlocker = !link
    ? (githubAsked && details.github === undefined && !githubView && !githubProject.error ? "Checking the project’s GitHub setting…" : null)
    : !githubSupported ? GITHUB_UPDATE
    : !githubView ? githubProject.error ?? "Checking the project’s GitHub setting…"
    : githubView.settings.mode === "off" ? "Turn on the GitHub workflow for this project in Tasks › GitHub first."
    : readOnlyLink(link) && mode !== "plan" ? planOnlyText(link)
    : fixBlocker ? `Fix with: ${fixBlocker}`
    : null;
  const choiceBlocker = creates && agent ? agentChoiceBlocker(harness, agent) ?? (agent.launchPolicy != null && !launchSupported ? LAUNCH_POLICY_UPDATE : null) ?? (sentBranch !== null && !branchSupported ? TASK_BRANCH_UPDATE : null) ?? launchBlocker ?? githubBlocker : null;
  // The live reading once the computer answers; until then what was captured
  // when the project was added.
  const checkedOut = branches.read ? branches.listing?.current ?? null : project.vcs?.branch ?? null;
  const dirty = branches.read ? branches.listing?.dirty_files ?? 0 : project.vcs?.dirty_files ?? 0;
  const repository = !alone && (branches.read ? branches.listing !== null : !!project.vcs);
  const newRequestBlocked = !online
    ? `Connect to ${profile.name} to send. Your draft stays on this device.`
    : !grants?.tasks
    ? `This device isn’t allowed to work with tasks on ${profile.name}.`
    : controlReason
      ? controlReason
    : carryingPlan && (task?.mode !== "plan" || task.state !== "completed" || run?.state !== "completed")
      ? "Only a finished Plan can be carried out. Open the current task to review it."
    : carryingPlan && route.plan && !latestPlan
      ? "Checking that this is still the latest plan…"
    : carryingPlan && route.plan && (latestPlan?.runId !== route.plan.runId || latestPlan.text !== route.plan.text)
      ? "A newer plan was proposed. Open the task and carry out its latest plan."
    : carryingPlan && writer
      ? `${writerHoldText(writer, project.name)} Wait for it to finish before carrying out the plan.`
    : taskChanged
      ? "The task changed. Review the update before continuing."
    : taskRecipient.blocked
      ? "Confirm which agent should receive this message."
    : choiceBlocker
      ? choiceBlocker
    : creates && !agent
      ? "No agent on this computer can start work from the device yet."
      : (creates || carryingPlan) && support.availability.state !== "available"
        ? `${modeLabel(mode)} isn’t available with ${harness?.display_name ?? "this agent"}. Choose another mode or agent.`
        : later && annotation && !queueMarkup
        ? QUEUE_MARKUP_UPDATE
        : handingBack && !grants?.control_input
        ? `This device can’t resume the agent on ${profile.name}.`
        : stale
        ? (chosenOption?.unavailable ?? "The task changed while you were reviewing. Go back to choose how to send.")
        : !text.trim() && !annotation
          ? "Write what you want done first."
          : null;
  const blocked = controlReason ?? (!online ? newRequestBlocked : unresolved
    ? !grants?.tasks
      ? newRequestBlocked
      : repeatable(unresolved, app.session)
        ? retryRecipient.blocked ? "Confirm the agent for the earlier request before repeating it." : null
        : `Check whether ${profile.name} received the last send first, or choose Send anyway.`
    : newRequestBlocked);

  // Where, who, what access and when, beside the button that starts it.
  const summary = creates && agent ? startSummary({
    project: project.name, alone, agent: harness?.display_name ?? agent.harness, harness: agent.harness,
    policy: agent.launchPolicy, mode, placement, authorization, paused: queue?.paused ?? false,
  }) : null;
  const primary = creates ? (placement === "queue" ? "Add to Next up" : "Start task") : carryingPlan ? "Start Work" : later ? "Put in Next up" : intent === "continue" ? "Continue" : intent === "interrupt_and_send" ? "Stop and send" : handingBack ? "Send and resume" : "Send";

  const submit = async (anyway = false) => {
    if (sending || attempt?.phase === "sending") return;
    if (unresolved && !anyway) {
      if (!blocked) void again(unresolved);
      return;
    }
    if (newRequestBlocked) return;
    const prompt = text.trim();
    if (creates && !prompt) {
      setRequestError("Say what you want done.");
      requestField.current?.focus();
      requestField.current?.scrollIntoView?.({ block: "nearest" });
      return;
    }
    let req: SendRequest;
    if (creates) {
      // A project's last finder and fixer are offered again next time.
      if (agent && (link?.type === "find" || link?.type === "triage")) rememberRole(profile.profileId, projectId, "find", choiceOf(agent));
      if (link?.type === "find") rememberRole(profile.profileId, projectId, "fix", link.fix_with);
      const p: TaskPlacement = placement === "queue" ? { mode: "queue", position: null, authorization } : { mode: "start_now" };
      // On its own, the task's new project is named for it and made with it.
      const place: TaskPlace | null = alone ? { type: "one_off", name: title.trim() || firstLine(prompt, 60), date: localDate() } : null;
      const target = alone ? uuidv7() : projectId;
      req = {
        projectId,
        draftTaskId: taskId,
        text,
        annotationId: draft?.annotationId ?? null,
        label: primary,
        build: (bundle) =>
          createTaskCommand({
            projectId: target,
            prompt,
            title: title.trim() || null,
            agent: agent!,
            mode,
            placement: p,
            mission: missionFrom(prompt || title, stop, details.bounds, details.completionChecks),
            bundle,
            branch: sentBranch,
            start: start?.start ?? null,
            place,
            browser: browserSupported && mode === "work" ? details.browser !== false : null,
            github: link?.type === "find" && fixWith ? { ...link, fix_with: fixWith } : link,
          }),
      };
    } else if (continues) {
      req = {
        projectId,
        draftTaskId: taskId,
        text,
        annotationId: draft?.annotationId ?? null,
        label: carryingPlan ? "Carry out this plan" : later ? "Put in Next up" : "Continue",
        expectedRevision: reviewedTask?.revision,
        build: (bundle): Command => later ? { command: "queue_continuation", task_id: taskId!, text: prompt, ...(bundle ? { bundle } : {}), authorization: "auto_start" } : ({ command: "resume_task", task_id: taskId!, text: carryingPlan && route.plan ? withPlan(prompt, route.plan) : prompt || null, ...(carryingPlan ? { mode: "work" as const } : {}), ...(carryingPlan && route.plan ? { carried_plan: { run_id: route.plan.runId, text: route.plan.text } } : {}), ...(bundle ? { bundle } : {}) }),
      };
    } else {
      req = {
        projectId,
        draftTaskId: taskId,
        text,
        annotationId: draft?.annotationId ?? null,
        label: DELIVERY[intent].label,
        build: (bundle) => ({ command: "send_message", task_id: taskId!, text: prompt, bundle, delivery: intent }),
      };
    }
    const label = UNDERWAY[primary]!;
    setSending({ label, detail: null });
    setDone(true);
    const out = await send(app, req, (detail) => setSending({ label, detail }));
    if (out.ok) await giveBack();
    setSending(null);
    if (!out.ok) {
      setDone(false);
      reportFailure(out.attempt.detail);
      if (stillHere() && out.attempt.phase === "rejected") requestField.current?.focus();
    }
  };

  /** Once the drawing is in, the target's input goes back to the run. */
  const giveBack = async () => {
    if (!handingBack || !task) return;
    const label = backTarget?.label ?? "The screen";
    try {
      // It acts only while someone watches its screen.
      if (await returnToAgent(app.session, execute, { taskId: task.task_id, runId: handingBack.runId, targetId: handingBack.targetId, view: backView, mine: backMine }))
        ui.push({ screen: "computer_use", taskId: task.task_id }, "control");
    } catch (e) {
      toast(`Your markup was sent, but ${label} didn’t resume the agent. ${describeError(e, profile.name)} Resume it from the task.`, { tone: "stop" });
    }
  };

  const again = async (a: SendAttempt) => {
    if (retryRecipient.blocked) return;
    setSending({ label: UNDERWAY[primary]!, detail: null });
    setDone(true);
    let out;
    try {
      out = await resend(app, projectId, taskId, a, primary);
      if (out.ok) await giveBack();
    } catch (e) {
      // Refused before anything left: the draft already says why.
      setSending(null);
      setDone(false);
      reportFailure((e as Error).message);
      return;
    }
    setSending(null);
    if (!out.ok) {
      setDone(false);
      reportFailure(out.attempt.detail);
      if (stillHere() && out.attempt.phase === "rejected") requestField.current?.focus();
    }
  };

  const recheck = async () => {
    if (!attempt) return;
    setChecking(true);
    try {
      await check(app, projectId, taskId, attempt);
    } catch {
      announce(`Couldn’t ask ${profile.name} yet. Try again when it’s connected.`, "assertive");
    } finally {
      setChecking(false);
    }
  };

  const account = harness?.account;
  const queuePaused = queue?.paused ?? false;
  // Where linked work goes when no branch is chosen: the link names it.
  const prefix = githubView?.settings.branch_prefix ?? "";
  const writesLink = !!link && !readOnlyLink(link);
  const linkBranch = link?.type === "pr" ? "The pull request’s own" : writesLink && !sentBranch ? "A new branch" : null;
  const linkBranchNote = link?.type === "pr" ? `Continues #${link.number}’s branch, or a new one when it’s on a fork the agent can’t push to.`
    : !writesLink || sentBranch ? null
    : link?.type === "issue" ? `Named for issue #${link.number}${prefix ? `, starting “${prefix}”` : ""}.`
    : `The agent names it${prefix ? `, starting “${prefix}”` : ""}${link?.type === "stack" ? `, from #${link.number}’s branch` : ""}.`;
  const githubBranch = githubView?.repo?.default_branch ?? "main";
  const pickLink = (l: GithubLink | null) => {
    setDetails({ github: l });
    if (readOnlyLink(l)) {
      if (mode !== "plan") setMode("plan");
      const finder = lastRole(profile.profileId, projectId, "find");
      if (finder && !draft?.agent) ui.saveDraft(projectId, taskId, { agent: { ...finder } });
    } else if (readOnlyLink(link) && mode === "plan") setMode("work");
  };

  return (
    <Screen
      className="review-screen"
      title={TITLE[intent]}
      subtitle={task ? task.title : project.name}
      footer={
        <div className="review-foot">
          {(requestError || attempt?.phase === "rejected") && !sending && <p className="review-foot__why" role="alert">Not sent · {requestError ?? attempt?.detail}</p>}
          {sending && (
            <p className="review-foot__progress" role="status">
              <span className="stateline__spinner" aria-hidden />
              {sending.detail ?? `Sending to ${profile.name}`}
            </p>
          )}
          {controlReason && (unlockable ? <Unlock />
            : <Button onClick={() => ui.push({ screen: "control" }, "control")}>Open Control</Button>)}
          {!sending && (blocked || unresolved) && (
            <p className="review-foot__why" role="status">
              {blocked ?? `${profile.name} may already have this. ${primary} repeats the same request, so it can’t happen twice.`}
            </p>
          )}
          {summary && (
            <p className={summary.risk ? "review-foot__summary launch-line__risk" : "review-foot__summary"}>
              {summary.risk && <TriangleAlert aria-hidden strokeWidth={2.2} />}
              <span>{summary.text}</span>
            </p>
          )}
          {/* Said right above the button that makes the switch. */}
          {fromMode && agent && <ModeSwitch from={fromMode} to={mode} flags={agent} />}
          {carryingPlan && task && <ModeSwitch from={task.mode ?? "work"} to="work" who="It"
            flags={task.launch_policy ? { harness: task.harness, launchPolicy: task.launch_policy } : null} />}
          <Button
            variant={intent === "interrupt_and_send" ? "warn" : "primary"}
            size="lg"
            block
            pending={sending !== null}
            disabled={blocked !== null || attempt?.phase === "sending"}
            onClick={() => void submit()}
          >
            {sending?.label ?? primary}
          </Button>
        </div>
      }
    >
      {going && <SendWhenClear clear={capabilitiesKnown && blocked === null && !unresolved && !requestError && !sending && attempt?.phase !== "sending"} onSend={() => void submit()} setGoing={setGoing} />}
      <div className="review">
        {!creates && <TaskRecipient recipient={taskRecipient} />}
        {repeatable(attempt, app.session) && <TaskRecipient recipient={retryRecipient} retry />}
        {taskChanged && task && <Notice tone="warn" title="The task changed" action={<Button onClick={() => { setReviewedTask(task); setReviewedRun(run); }}>Review current task</Button>}>
          {reviewedTask?.title !== task.title && <p>Title: {reviewedTask?.title} → {task.title}</p>}
          <p>Was {reviewedTask ? taskBadge(reviewedTask, reviewedRun).label : "not loaded"}; now {taskBadge(task, run).label}. {task.status_line}</p>
          <p>Your message is still here. Nothing will be sent until you choose Continue again.</p>
        </Notice>}
        {attempt && attempt.phase !== "rejected" && !sending && (
          <AttemptNotice body={attempt?.body}
            phase={attempt.phase}
            detail={attempt.detail}
            checking={checking}
            canCheck={attempt.commandId !== null}
            repeatable={repeatable(attempt, app.session)}
            retryBlocked={repeatable(attempt, app.session) ? retryRecipient.blocked : taskRecipient.blocked}
            onCheck={() => void recheck()}
            onRetry={() => void (repeatable(attempt, app.session) ? again(attempt) : submit(true))}
            onDismiss={() => ui.saveDraft(projectId, taskId, { attempt: null })}
          />
        )}

        <Section title={creates ? "What you’re asking" : "Your message"}>
          <TextArea ref={requestField} aria-invalid={!!requestError || attempt?.phase === "rejected"} label={creates ? "What you’re asking" : "Your message"} hideLabel placeholder={creates ? "What should the agent do?" : undefined} value={text} onChange={setText} maxRows={10} rows={3} className="review__text" />
          {fromComment && !requestError && <p className="review__sub">{commented > 1
            ? `From your first markup comment; all ${commented} go with the markup, as listed below. You can edit this before sending.`
            : "From your first markup comment. You can edit it before sending."}</p>}
          {!creates && commented > 0 && <p className="review__sub">{commented === 1 ? "Your comment travels" : `Your ${commented} comments travel`} with the markup.</p>}
          {requestError && <p role="alert">{requestError}</p>}
          {annotation && (
            <div className="review__attach">
              <button type="button" className="review__attach-open" onClick={() => ui.push({ screen: "annotate", annotationId: annotation.id, intent, ...(route.handBack ? { handBack: route.handBack } : {}) })}>
                <Paperclip aria-hidden strokeWidth={2.1} />
                <span>
                  <span className="review__attach-what">Markup</span>
                  <span className="review__attach-desc">{describe(summarize(annotation))}</span>
                </span>
                <span className="review__attach-edit">Edit</span>
              </button>
              <button
                type="button"
                className="review__attach-x"
                aria-label="Remove the markup"
                onClick={() => removeMarkup(ui, store, projectId, taskId, annotation.id)}
              >
                <X aria-hidden strokeWidth={2.2} />
              </button>
            </div>
          )}
          {annotation && <MarkupComments annotation={annotation} />}
        </Section>

        {carryingPlan && route.plan && (
          <Section title={`Plan from run ${route.plan.number}`} id="review-plan">
            <blockquote className="plan-quote"><Markdown text={route.plan.text} /></blockquote>
            <p className="review__sub">Sent after your message, as the agent wrote it.</p>
          </Section>
        )}

        {annotation && <ReproductionReview draft={annotation} />}

        <Section title="Where">
          {intent === "new_task" && taskId === null && hostProject && !hostProject.one_off && launchersSupported && (
            <Segmented
              label="Where it works"
              value={alone ? "one_off" : "project"}
              onChange={(place) => setDetails({ place })}
              options={[
                { value: "project", label: `In ${hostProject.name}` },
                { value: "one_off", label: "On its own" },
              ]}
            />
          )}
          <dl className="review__facts">
            {alone ? (
              <div className="review__fact">
                <dt>
                  <FolderPlus aria-hidden strokeWidth={2} />
                  Folder
                </dt>
                <dd>
                  <span className="review__strong">A new folder, {localDate()}-{slug(title.trim() || firstLine(text, 60))}</span>
                  {oneOffs.catalog?.one_offs && <span className="review__mono"><PathText path={oneOffs.catalog.one_offs} /></span>}
                  <span className="review__sub">Listed under One-offs. You can make it a project later.</span>
                </dd>
              </div>
            ) : (
              <div className="review__fact">
                <dt>
                  <FolderGit2 aria-hidden strokeWidth={2} />
                  Project
                </dt>
                <dd>
                  <span className="review__strong">{project.name}</span>
                  {creates && taskId === null && selectedProjectId !== projectId && <span className="review__sub">Starting this task switches to {project.name}.</span>}
                  <span className="review__mono"><PathText path={project.host_path} /></span>
                </dd>
              </div>
            )}
            {(repository || chosenBranch !== null) && (
              <div className="review__fact">
                <dt>
                  <GitBranch aria-hidden strokeWidth={2} />
                  Branch
                </dt>
                <dd>
                  {creates ? (
                    <Chip className="review__agent review__branch" onClick={() => { branches.refresh(); setChoosingBranch(true); }} aria-haspopup="dialog" aria-label={`Branch: ${linkBranch ?? chosenBranch?.name ?? "All branches"}${chosenBranch?.new ? ", new" : ""}`} disabled={!branchSupported || link?.type === "pr"} icon={chosenBranch?.new ? GitBranchPlus : undefined}>
                      <span>{linkBranch ?? chosenBranch?.name ?? "All branches"}</span>
                      <ChevronDown className="composer__chip-chev" aria-hidden />
                    </Chip>
                  ) : (
                    <span className="review__strong">{chosenBranch?.name ?? "All branches"}{chosenBranch?.new ? " (new)" : ""}</span>
                  )}
                  {creates && capabilitiesKnown && !branchSupported
                    ? <span className="review__sub">{TASK_BRANCH_UPDATE}</span>
                    : linkBranchNote ? <span className="review__sub">{linkBranchNote}</span>
                    : (repository || chosenBranch?.new) && <span className="review__sub">{branchStatus(chosenBranch, checkedOut, dirty, repository, !!chosenBranch?.new && !!branches.listing?.branches.includes(chosenBranch.name))}</span>}
                </dd>
              </div>
            )}
            {creates && !alone && githubSupported && (githubOn || link !== null) && (
              <div className="review__fact">
                <dt>
                  <GitPullRequest aria-hidden strokeWidth={2} />
                  GitHub
                </dt>
                <dd>
                  <Chip className="review__agent" onClick={() => setChoosingGithub(true)} aria-haspopup="dialog" aria-label={`GitHub: ${linkWords(link)}`}>
                    <span>{linkWords(link)}</span>
                    <ChevronDown className="composer__chip-chev" aria-hidden />
                  </Chip>
                  <span className="review__sub">{linkDetail(link, sentBranch?.name ?? null)}</span>
                  {writesLink && githubView?.settings.mode === "through_merge" && githubView.settings.warning_answered && <span className="review__sub github-reminder">{reminderText(githubBranch)}</span>}
                  {writesLink && githubView?.settings.mode === "drafts" && <span className="review__sub">You mark it ready and merge it.</span>}
                  {link && githubView?.settings.mode === "off" && (
                    <Button size="sm" variant="quiet" onClick={() => ui.push({ screen: "github", projectId })}>Turn on GitHub</Button>
                  )}
                </dd>
              </div>
            )}
            <div className="review__fact">
              <dt>
                <Monitor aria-hidden strokeWidth={2} />
                Computer
              </dt>
              <dd>
                <span className="review__strong">{profile.name}</span>
              </dd>
            </div>
            {task && (
              <div className="review__fact">
                <dt>
                  <Bot aria-hidden strokeWidth={2} />
                  {creates ? "From" : "Task"}
                </dt>
                <dd>
                  <span className="review__strong">{task.title}</span>
                  {creates && <span className="review__sub">This becomes a separate task. “{firstLine(task.title, 40)}” isn’t changed.</span>}
                </dd>
              </div>
            )}
          </dl>
          {writesLink && githubView?.settings.mode === "through_merge" && !githubView.settings.warning_answered && (
            <ThroughMergeWarning view={githubView} busy={warning.busy} online={online && !!grants?.admin} onAnswer={(keep) => {
              const a = answerWarning(githubView, keep);
              void warning.run(a.action, a.label, a.done).then(githubProject.reload);
            }} />
          )}
        </Section>

        {creates ? (
          <Section title={link?.type === "find" ? "Find with" : link?.type === "triage" ? "Sort with" : "Agent"}>
            {agent ? (
              <>
                {!harnesses.some(canStart) && <Notice tone="warn" title="No agent can start work here yet" action={<ReadinessCheck />}>
                  Your selected agent is kept. Check its setup or choose another agent.
                </Notice>}
                {start ? (
                  <div className="review__attach review__start">
                    <button type="button" className="review__attach-open" disabled={!launchersSupported} onClick={() => setChoosingStart(true)} aria-haspopup="dialog">
                      {startKind(start) === "scratch" ? <NotebookPen aria-hidden strokeWidth={2} /> : startKind(start) === "agent" ? <Bot aria-hidden strokeWidth={2} /> : <Sparkles aria-hidden strokeWidth={2} />}
                      <span>
                        <span className="review__strong">Starts from {startedLabel(start)}</span>
                        <span className="review__sub">{startNote(start, harness)}</span>
                      </span>
                      {launchersSupported && <span className="review__attach-edit">Change</span>}
                    </button>
                    <button type="button" className="review__attach-x" aria-label={`Don’t start from ${start.name}`} onClick={() => setDetails({ start: null })}>
                      <X aria-hidden strokeWidth={2.2} />
                    </button>
                  </div>
                ) : launchersSupported && (
                  <Chip icon={Sparkles} className="review__agent" onClick={() => setChoosingStart(true)} aria-haspopup="dialog">
                    <span>Start from</span>
                    <ChevronDown className="composer__chip-chev" aria-hidden />
                  </Chip>
                )}
                <Chip icon={Bot} className="review__agent" onClick={() => setChoosing(true)} aria-haspopup="dialog">
                  <span>{agentLabel(harnesses, agent)}</span>
                  <ChevronDown className="composer__chip-chev" aria-hidden />
                </Chip>
                {/* Its warning is under Mode, with what Work may do. */}
                <LaunchLine className="review__sub" harness={agent.harness} policy={agent.launchPolicy} mode={mode} risk={false} />
                {account?.state === "signed_in" && (
                  <p className="review__sub">
                    Signed in{account.account_label ? ` as ${account.account_label}` : ""}
                    {account.plan ? ` · ${account.plan}` : ""}. Uses that account’s plan; nothing is billed by this app.
                  </p>
                )}
                {account?.state === "signed_out" && (
                  <Notice tone="warn" title={`${harness?.display_name} isn’t signed in`}>
                    {account.remedy}
                  </Notice>
                )}
                {account?.state === "unknown" && (
                  <Notice tone="unknown" title="Sign-in not confirmed">
                    {account.reason}
                  </Notice>
                )}
              </>
            ) : (
              <Notice tone="warn" title="No agent can start work here yet" action={<ReadinessCheck />}>
                Check which agents are installed and signed in on {profile.name}.
              </Notice>
            )}
          </Section>
        ) : (
          <Section title="How">
            {task?.conversation && !carryingPlan && (
              <p className="review__permission">
                <span className="review__strong">What it may reach.</span> {describeAccess(heldAccess(task, accessIsHeld), project, projects)}
              </p>
            )}
            {task && !task.conversation && !carryingPlan && (task.mode ?? "work") !== "work" && (
              <p className="review__permission">
                <span className="review__strong">Stays in {modeLabel(task.mode)} mode, read-only.</span> {readOnlyLink(task.github?.link) ? "What it reports is acted on by the issues and fixes it leads to." : task.mode === "plan" ? "To let it change files, choose Carry out this plan." : "To let it change files, start a new task in Work mode."}
              </p>
            )}
            {carryingPlan ? (
              <p className="review__permission"><span className="review__strong">Mode: Work.</span> Continues this task and its native session with the plan above. It can now change files. Starting is your permission to leave read-only Plan mode.</p>
            ) : intent === "continue" ? (
              <p className="review__permission">{chosenOption?.detail ?? "Starts a new run of this task with your message."}</p>
            ) : (
              <p className="review__permission">
                <span className="review__strong">{DELIVERY[intent].label}.</span> {DELIVERY[intent].detail}
              </p>
            )}
            {handingBack && backTarget && (
              <p className="review__permission">
                <span className="review__strong">Then resumes the agent on {backTarget.label}.</span> It continues with your markup and a fresh observation.
              </p>
            )}
            {carryingPlan && task && <p className="review__permission">{agentLabel(harnesses, { harness: task.harness, model: task.model, effort: task.effort })}</p>}
            {harness && (carryingPlan || (task?.mode ?? "work") === "work") && <Permission risky={!!task && !!launchRisk(task.harness, task.launch_policy)}>{carryingPlan && !task?.launch_policy && harness.kind === "codex" ? `Can write files in this project with Codex’s workspace sandbox. Asks on ${grants?.approvals ? "this device" : profile.name} before actions that need more permission.` : permissionText(harness, profile.name, grants?.approvals ?? false, task?.launch_policy)}</Permission>}
          </Section>
        )}

        {link?.type === "find" && (
          <Section title="Fix with">
            <Chip icon={Bot} className="review__agent" onClick={() => setChoosingFix(true)} aria-haspopup="dialog">
              <span>{agentLabel(harnesses, link.fix_with)}</span>
              <ChevronDown className="composer__chip-chev" aria-hidden />
            </Chip>
            {fixLaunchSupported ? <LaunchLine className="review__sub" harness={link.fix_with.harness} policy={fixWith?.launch_policy} /> : <p className="review__sub">{FIX_LAUNCH_UPDATE}</p>}
            <p className="review__sub">
              {findThen === "file_only" ? "It only files issues; this agent fixes nothing unless you start a fix."
                : `${findMost ? `It files up to ${findMost}. ` : ""}Each issue it files becomes “Fix #N” in Next up with this agent.`}
              {findThen === "ask_first" ? " Each waits for you to start it." : findThen === "by_itself" ? " They start one after another by themselves." : ""}
              {/* Starting a finder that fixes by itself resumes Next up, or they would wait unseen. */}
              {queue?.paused && findThen !== "file_only" && (findThen === "ask_first" || placement === "queue" ? " Next up is paused, so they wait until you resume it." : " Next up is paused, so starting this Find resumes it.")}
            </p>
          </Section>
        )}

        {creates && agent && (
          <Section title="Mode">
            <ModePicker harness={harness} value={mode} onChange={setMode}
              only={link && readOnlyLink(link) ? { mode: "plan", why: `${planOnlyWhy(link)} To ask or work instead, change GitHub above.` } : undefined} />
            {mode === "work" ? <>
              <Permission risky={!!launchRisk(agent.harness, agent.launchPolicy)}>{permissionText(harness, profile.name, grants?.approvals ?? false, agent.launchPolicy)}</Permission>
              {browserSupported && (
                <label className="review__check">
                  <input type="checkbox" checked={details.browser !== false} onChange={(e) => setDetails({ browser: e.target.checked })} />
                  <span>
                    <span className="review__strong">Browser</span>
                    <span className="review__permission">Lets it use {PRODUCT_NAME}’s own browser on websites, signed in with the agent logins you allow it.</span>
                  </span>
                </label>
              )}
            </> : (
              support.availability.state === "available" && (
                <>
                  <p className="review__permission">
                    <span className="review__strong">Read-only on {profile.name}.</span> {sentence(support.enforcement)}
                  </p>
                  {harness?.kind === "claude_code" && (
                    <p className="review__mode-note">
                      In this mode Claude Code starts without the project’s CLAUDE.md, skills and hooks, so it may know less about how the project is run.
                    </p>
                  )}
                </>
              )
            )}
          </Section>
        )}

        {creates && alone && (
          <Section title="When">
            <p className="review__sub">Starts now. A task on its own has no Next up to wait in.</p>
          </Section>
        )}
        {creates && !alone && (
          <Section title="When">
            <Segmented
              label="When to start"
              value={placement}
              onChange={setPlacement}
              options={[
                { value: "start_now", label: "Start now", disabled: writerBlocks },
                { value: "queue", label: "Add to Next up" },
              ]}
            />
            {writer && (writerBlocks || placement === "start_now") && (
              <div className="review__held">
                <p className="review__sub">
                  {writerBlocks ? writerHoldText(writer, project.name) : writerHoldBesideText(writer, project.name, modeLabel(mode))}
                </p>
                {writer.task && (
                  <Button size="sm" variant="secondary" trailingIcon={ChevronRight} onClick={() => openTask(writer.task!)}>
                    {writer.unresolved ? "Check it" : "Open it"}
                  </Button>
                )}
              </div>
            )}
            {placement === "queue" && (
              <>
                {/* The group carries the same name, so the visible label isn't read twice. */}
                <div className="field review__auth">
                  <span className="field__label" aria-hidden>When its turn comes</span>
                  <Segmented
                    label="When its turn comes"
                    value={authorization}
                    onChange={setAuthorization}
                    options={[
                      { value: "ask_before_start", label: "Ask me first" },
                      { value: "auto_start", label: "Start by itself" },
                    ]}
                  />
                </div>
                <p className="review__sub">
                  {authorization === "ask_before_start"
                    ? "When it reaches the front, you get a request on this device to start it."
                    : `When it reaches the front, ${profile.name} starts it without asking again. This permission lasts a day.`}
                  {queuePaused ? authorization === "auto_start" ? " Next up is paused in this project; adding this resumes it." : " Next up is paused in this project; nothing starts until you resume it." : ""}
                </p>
              </>
            )}
          </Section>
        )}

        {creates && (
          <details className="review__optional" open={optionalOpen}>
            <summary>Optional: title, boundaries, checks, when to stop</summary>
            <div className="review__optional-fields">
              <TextField label="Title" value={title} onChange={setTitle} placeholder={firstLine(text, 60) || "Named from your request"} />
              <TextArea label="Boundaries" value={details.bounds ?? ""} onChange={(bounds) => setDetails({ bounds })}
                placeholder="One boundary per line" hint="Requested instructions. The computer does not enforce these boundaries." rows={2} />
              <TextArea label="Completion checks" value={details.completionChecks ?? ""} onChange={(completionChecks) => setDetails({ completionChecks })}
                placeholder="One check per line" hint="Requested checks to report when the task finishes." rows={2} />
              <TextField
                label="Stop when"
                value={stop}
                onChange={setStop}
                placeholder="e.g. the tests pass, or before touching the database"
                hint="Given to the agent as your stopping point. The computer can’t enforce it."
              />
            </div>
          </details>
        )}

        {!grants?.approvals && (
          <Notice tone="info" title="Approvals happen on the computer">
            This device isn’t allowed to answer approval requests, so anything the agent asks will wait on {profile.name}.
          </Notice>
        )}
      </div>

      {creates && agent && project && (
        <AgentSheet open={choosing} onClose={() => setChoosing(false)} project={project} value={agent} mode={mode} onChange={(a) => ui.saveDraft(projectId, taskId, { agent: a })} />
      )}
      {creates && launchersSupported && (
        <StartFromSheet
          open={choosingStart}
          onClose={() => setChoosingStart(false)}
          projectId={alone ? null : projectId}
          onChoose={(s, a) => { setChoosingStart(false); setDetails({ start: s }); if (a) ui.saveDraft(projectId, taskId, { agent: a }); }}
        />
      )}
      {creates && !alone && githubSupported && (
        <LinkSheet open={choosingGithub} onClose={() => setChoosingGithub(false)} projectId={projectId} repo={githubView?.repo ?? null}
          value={link} stackOn={sourcePr} fixWith={fixDefault} onPick={pickLink} />
      )}
      {link?.type === "find" && project && (
        <AgentSheet open={choosingFix} onClose={() => setChoosingFix(false)} project={project} mode="work" launchFlags={fixLaunchSupported}
          value={{ ...choiceOf(link.fix_with), ...(link.fix_with.launch_policy ? { launchPolicy: link.fix_with.launch_policy } : {}) }}
          onChange={(a) => setDetails({ github: { ...link, fix_with: fixChoiceOf(a, fixLaunchSupported, link.fix_with) } })} />
      )}
      {creates && (
        <BranchSheet open={choosingBranch} onClose={() => setChoosingBranch(false)} branches={branches} chosen={chosenBranch} onPick={(branch) => setDetails({ branch })} computer={profile.name} />
      )}
    </Screen>
  );
}
