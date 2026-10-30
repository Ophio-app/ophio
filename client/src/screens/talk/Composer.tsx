// SPDX-License-Identifier: Apache-2.0
import { useCrampedLayout, useTightLayout } from "../../shell/keyboard";
import { SavedRetryDrafts } from "../work/RetryRequest";
import { unstartedQueueItem } from "../work/queueStatus";
import { pendingQueueItems, queueItemState } from "../work/useQueue";
//
// Where the person writes to the agent. It offers only what the computer
// and the agent can do for the task on screen, keeps the text on the device
// until the computer accepts it, and says plainly what happened when it did
// not. A new task, a Next up item or anything with markup goes through
// Request review first; a plain message to a running task is sent from here.
// In a conversation it is a chat's composer: the first message starts it, a
// reply goes at once, the model can change between messages, and it can be
// turned into a task.

import { useCarryPlan, reviewPlan } from "./carryPlan";
import { Bot, ChevronDown, ChevronRight, GitPullRequest, Hammer, ListChecks, ListPlus, NotebookPen, Paperclip, ScrollText, SendHorizontal, SlidersHorizontal, Sparkles, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { shallowEqual, useApp, useHost, useLink, useUi } from "../../app/context";
import { draftKey, type AgentChoice, type ReviewIntent, type SendAttempt, type SourceStart } from "../../app/ui";
import { announce } from "../../design/Announcer";
import { Button, IconButton } from "../../design/Button";
import { Chip } from "../../design/Chip";
import { TextArea } from "../../design/Field";
import { ListRow } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import type { HarnessInfo, Project, Run, Task } from "../../protocol/types";
import { annotationStore, describe, summarize, useAnnotation } from "../../state/annotations";
import { TaskRecipient, useRetryRecipient, useTaskRecipient } from "./TaskRecipient";
import { AgentSheet } from "./AgentSheet";
import { TaskAgentChip, TaskAgentSheet, useTaskAgent } from "./TaskAgent";
import { agentLabel, resolveAgent, composerMode, defaultIntent, modeSupport, writerBlockLine, writerHold, writerQueueLine, HOLDER_TITLE, needsReview, sameIntent, sendLabel, type Intent, type IntentOption } from "./intent";
import { CONVERSATIONS_FEATURE, CONVERSATIONS_UPDATE, NEW_CONVERSATION, conversationSummary, intoTask, nextChoice, startConversation, turnModel } from "./conversation";
import { INTO_TASK_TEXT, IntoTaskSheet } from "./IntoTask";
import { availabilityText } from "../../design/Notice";
import { check, repeatable, resend, send, useAutoSettle, type SendRequest } from "./send";
import { READ_ONLY_TURN_FEATURE, summarizable, summaryRequest } from "./summarize";

import { continuationBlocker } from "../control/readiness";
import { Unlock, useUnlockable } from "../control/Unlock";
import { continuationSelection, planReviewSelection } from "./continuationSelection";
import { removeMarkup } from "./removedMarkup";
import { AttachSheet } from "./Attach";
import { useStore } from "../../state/store";
import { LAUNCHERS_FEATURE, setDraftStart, startedLabel } from "../../app/launchers";
import { QUEUE_MARKUP_FEATURE, QUEUE_MARKUP_UPDATE } from "../../app/queueMarkupFeature";
import { StartFromSheet } from "../launch/StartFrom";
import { accessHeld, defaultAccess } from "./access";

function reviewIntent(i: Intent): ReviewIntent {
  return i.kind === "message" ? i.delivery : i.kind === "continue_later" ? "continue" : i.kind;
}

function placeholder(i: Intent, agent: string, project: string): string {
  switch (i.kind) {
    case "new_task":
      return `What should ${agent} do in ${project}?`;
    case "next_up":
      return "Describe a task for Next up";
    case "continue":
      return "What should it do next?";
    case "continue_later":
      return "What should it do once the project is free?";
    case "message":
      return i.delivery === "steer" ? `Tell ${agent} while it works` : i.delivery === "next_turn" ? "Message for after this turn" : "Stop the turn and say…";
  }
}

/** Shorter visible placeholder; the send-options chip already says how it
 * goes. Where the text is large for the screen, only a word or two fits
 * beside the tools. */
function shortPlaceholder(i: Intent, agent: string, tight: boolean, separate = false): string {
  switch (i.kind) {
    case "new_task":
      return tight ? "New task" : separate ? "Describe a separate task" : "Describe the task";
    case "next_up":
      return tight ? "Next up" : "Task for Next up";
    case "continue":
    case "continue_later":
      return "What next?";
    case "message":
      return i.delivery === "interrupt_and_send" ? "Stop and say…" : tight ? "Message" : `Message ${agent}`;
  }
}

/** The end of the bar that is not in use: out of sight, reach and speech. */
const away = { inert: true, "aria-hidden": true } as const;

export function Composer({
  project,
  task,
  run,
  harness,
  agent,
  onSent,
  conversation = false,
}: {
  onSent?: () => void;
  /** With no task: the first message starts a conversation, not a task. */
  conversation?: boolean;
  project: Project;
  task: Task | null;
  run: Run | null;
  /** The task's harness, or the one a new task would use. */
  harness: HarnessInfo | null;
  /** For a new task: the agent it would start with (null when none can). */
  agent: AgentChoice | null;
}) {
  const app = useApp();
  const [planError, setPlanError] = useState<string | null>(null);
  const plan = useCarryPlan(task);
  const { ui, profile } = app;
  const projectId = project.project_id;
  // A new conversation, or one going on: messages go straight to the agent.
  const fresh = conversation && !task;
  const talking = fresh || !!task?.conversation;
  const taskId = task?.task_id ?? (fresh ? NEW_CONVERSATION : null);
  const draft = useUi((s) => s.drafts[draftKey(projectId, taskId)] ?? null);
  const harnesses = useHost((s) => s.harnesses);
  const queue = useHost((s) => s.queues[projectId]);
  const queued = task ? unstartedQueueItem(task, run ? [run] : [], queue) : undefined;
  const ready = queued?.state.state === "awaiting_start" && !queue?.paused;
  const canWork = useHost((s) => s.grants?.tasks ?? false);
  const link = useLink();
  const store = annotationStore(profile.profileId);
  const annotationDraft = useAnnotation(store, draft?.annotationId ?? null);
  // Opening Annotate alone is not an attachment. Keep the empty workspace
  // available to reopen without a chip or a detour through Review.
  const annotation = annotationDraft?.sources.length ? annotationDraft : null;
  const field = useRef<HTMLTextAreaElement>(null);
  const readinessHint = useRef<HTMLParagraphElement>(null);

  const writer = useHost((s) => writerHold(s.projects[projectId]?.active_writer, s), shallowEqual);
  const mode = useMemo(() => composerMode(task, run, harness, writer, project.name), [task, run, harness, writer, project.name]);
  const options: IntentOption[] = mode.mode === "running" || mode.mode === "ended" ? mode.options : [];
  const [chosen, setChosen] = useState<Intent | null>(null);
  const [picking, setPicking] = useState(false);
  const [toolsOpen, setToolsOpen] = useState(false);
  const [attaching, setAttaching] = useState(false);
  const [choosingAgent, setChoosingAgent] = useState(false);
  const [showingAgent, setShowingAgent] = useState(false);
  const [choosingStart, setChoosingStart] = useState(false);
  const [choosingModel, setChoosingModel] = useState(false);
  /** Turn into a task is open, with the words it opened with. */
  const [carrying, setCarrying] = useState<string | null>(null);
  const conversations = useHost((s) => !!s.capabilities?.features.includes(CONVERSATIONS_FEATURE));
  const launchers = useHost((s) => !!s.capabilities?.features.includes(LAUNCHERS_FEATURE));
  const readOnlyTurns = useHost((s) => !!s.capabilities?.features.includes(READ_ONLY_TURN_FEATURE));
  const queueMarkup = useHost((s) => !!s.capabilities?.features.includes(QUEUE_MARKUP_FEATURE));
  const [sending, setSending] = useState<{ detail: string | null } | null>(null);
  const [checking, setChecking] = useState(false);

  useEffect(() => setChosen(null), [taskId]);
  const continuation = useStore(continuationSelection(ui), s => s);
  useEffect(() => {
    if (continuation?.taskId === taskId) {
      setChosen({ kind: "continue" });
      field.current?.focus();
    }
  }, [continuation, taskId]);

  const readOnly = mode.mode === "waiting" || mode.mode === "reconcile" || mode.mode === "archived";
  const intent: Intent = (() => {
    if (readOnly || mode.mode === "new") return { kind: "new_task" };
    if (mode.mode === "starting") return { kind: "message", delivery: "next_turn" };
    // A way that can't be used now, such as Continue while another task
    // holds the project, gives way to the default, so an answer to the agent
    // can still wait in Next up.
    const ok = chosen && options.find((o) => sameIntent(o.intent, chosen) && !o.unavailable);
    return ok ? chosen! : defaultIntent(mode);
  })();
  // A message that starts a new task, including writing on a read-only one,
  // isn't read by this task's agent, so the chip only shows for this task.
  const taskAgent = useTaskAgent(intent.kind === "new_task" || intent.kind === "next_up" || talking ? null : task, run);
  const controlReason = useHost((s) => intent.kind === "continue" ? continuationBlocker(task, s.capabilities, s.targets, s.control, s.grants) : null);
  const unlockable = useUnlockable(intent.kind === "continue" ? task?.target_id : null);
  useEffect(() => {
    if (controlReason) readinessHint.current?.scrollIntoView?.({ block: "nearest" });
  }, [controlReason]);
  const option = options.find((o) => sameIntent(o.intent, intent));

  const text = draft?.text ?? "";
  const planContext = JSON.stringify([taskId, intent, draft]);
  const currentPlanContext = useRef({ key: planContext });
  if (currentPlanContext.current.key !== planContext) currentPlanContext.current = { key: planContext };
  const context = currentPlanContext.current;
  const mounted = useRef(false);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useEffect(() => { setPlanError(null); }, [planContext]);
  const carryPlan = task && plan ? ({ send }: { send?: boolean } = {}) => {
    setPicking(false); setToolsOpen(false); setPlanError(null);
    const stillCurrent = () => mounted.current && currentPlanContext.current === context;
    void reviewPlan(app, task, stillCurrent, { send }).catch((e: unknown) => {
      if (stillCurrent()) setPlanError(e instanceof Error ? e.message : "Couldn’t load the plan.");
    });
  } : undefined;
  const planSelection = planReviewSelection(ui);
  const planRequest = useStore(planSelection, s => s);
  useEffect(() => {
    if (planRequest?.taskId === taskId) {
      planSelection.set(null);
      carryPlan?.({ send: planRequest.send });
    }
  }, [planRequest, taskId, planSelection, carryPlan]);
  const hasContent = text.trim().length > 0 || annotation !== null;
  const attempt = draft?.attempt ?? null;
  const newDraft = useUi(s => s.drafts[draftKey(projectId, fresh ? NEW_CONVERSATION : null)]);
  const recipient = intent.kind === "new_task" ? resolveAgent(project, harnesses, newDraft?.agent ?? agent)?.harness : harness?.kind;
  const agentName = harnesses.find(h => h.kind === recipient)?.display_name ?? "the agent";
  const online = link.state === "online";
  const tight = useTightLayout();
  const cramped = useCrampedLayout();
  const newTaskNeedsAgent = intent.kind === "new_task" && mode.mode === "new" && !agent;
  const taskRecipient = useTaskRecipient(project, task);
  const retryRecipient = useRetryRecipient(projectId, taskId);
  const recipientBlocked = intent.kind !== "new_task" && intent.kind !== "next_up" && taskRecipient.blocked;
  const askUnavailable = fresh && harness ? availabilityText(modeSupport(harness, "ask").availability) : null;
  const disabledReason = recipientBlocked
    ? "Confirm which agent should receive this message."
    : !online && (fresh || !needsReview(intent, annotation !== null))
    ? `Connect to ${profile.name} to send. Your text stays on this device.`
    : !canWork
    ? `This device isn’t allowed to work with tasks on ${profile.name}.`
    : mode.mode === "starting"
      ? `${agentName} is starting. You can send once it’s working.`
      : newTaskNeedsAgent
        ? "No agent on this computer can start work from the device yet."
        : fresh && !conversations
        ? CONVERSATIONS_UPDATE
        : askUnavailable && !askUnavailable.ok
        ? `${agentName} can’t hold a read-only conversation here. ${askUnavailable.detail ?? ""}`.trim()
        : intent.kind === "continue_later" && annotation && !queueMarkup
          ? QUEUE_MARKUP_UPDATE
          : option?.unavailable ?? controlReason;
  const continueOption = options.find((o) => o.intent.kind === "continue");
  const blockedByWriter = (intent.kind === "continue" || intent.kind === "continue_later") && !!continueOption?.unavailable && writer?.task;
  // Held by the writer, a message can still wait in Next up for its turn.
  const waitsInNextUp = blockedByWriter && intent.kind === "continue_later";
  const canRepeat = repeatable(attempt, app.session);
  // A message for a run that has since ended. Repeating the same request is
  // still safe, but sending anew would start a new run, which is asked in Review.
  const overtaken = !!attempt?.delivery && (intent.kind === "continue" || intent.kind === "continue_later") && !canRepeat;
  // While the last send's fate is unknown, Send waits: the words may already
  // be there. The notice checks, or repeats that request under its id.
  const unresolved = attempt?.phase === "unknown";
  const unresolvedReason = unresolved ? `Check whether ${profile.name} received your last message first, or choose ${canRepeat ? "Send again" : "Send anyway"}.` : null;
  // Asking for a summary needs no writer, so it stays offered while another
  // task holds the project; it waits only for an earlier send to settle.
  const summary = readOnlyTurns && !!task && mode.mode === "ended" && summarizable(task, run, harness);
  const summaryBusy = !online || !canWork || sending !== null || attempt?.phase === "sending";
  const review = !fresh && needsReview(intent, annotation !== null);
  const label = fresh ? "Start conversation"
    : intent.kind === "message" && intent.delivery === "interrupt_and_send" && !review ? "Stop and send" : sendLabel(intent, annotation !== null, talking);
  // What the next message in a conversation uses, and carrying it into Work.
  const next = task?.conversation ? nextChoice(task, draft?.agent) : null;
  const carryable = !!task?.conversation && mode.mode === "ended";
  const carryBlocked = !task ? null
    : !online ? `Connect to ${profile.name} to start it.`
    : !canWork ? `This device isn’t allowed to work with tasks on ${profile.name}.`
    : writer && writer.task?.task_id !== task.task_id ? `${writerBlockLine(writer, HOLDER_TITLE)}.`
    : !harness || harness.capabilities.start_run.state !== "available" ? `${agentName} can’t start work on ${profile.name} now.`
    : unresolved || attempt?.phase === "sending" || sending ? "Wait for your last message to settle first."
    : null;

  useAutoSettle(projectId, taskId, attempt);

  const setText = (v: string) => ui.saveDraft(projectId, taskId, { text: v });

  /** Carry what was typed on a task over to a new task in the same project. */
  const moveToNewTask = () => {
    const target = ui.draft(projectId, null);
    const merged = [target?.text.trim(), text.trim()].filter(Boolean).join("\n\n");
    const carry = draft?.annotationId && !target?.annotationId ? draft.annotationId : null;
    // A start still in doubt on the new-task draft stays, so it is checked or
    // repeated under its id rather than sent again as another task.
    const inDoubt = target?.attempt?.phase === "unknown" || target?.attempt?.phase === "sending";
    ui.saveDraft(projectId, null, { text: merged, ...(draft?.agent ? { agent: draft.agent } : {}), ...(carry ? { annotationId: carry } : {}), ...(inDoubt ? {} : { attempt: null }) });
    if (carry) store.update(carry, (d) => ({ ...d, taskId: null }));
    ui.saveDraft(projectId, taskId, { text: "", attempt: null, ...(carry ? { annotationId: null } : {}) });
    ui.selectTask(projectId, null);
  };

  // A way to send is chosen for one message, like its text; the next one
  // starts again from the least disruptive way.
  const sent = () => {
    setChosen(null);
    onSent?.();
  };

  const go = async (anyway = false) => {
    if (!hasContent || disabledReason || sending || attempt?.phase === "sending") return;
    if (unresolved && !anyway) return;
    if (fresh) {
      if (!agent) return;
      const words = text.trim();
      const chosenAgent = agent;
      // What the bar above chose, on a computer that holds the agent to it.
      const access = accessHeld(app.session.state.get().capabilities) ? app.ui.get().drafts[draftKey(projectId, NEW_CONVERSATION)]?.access ?? defaultAccess(projectId) : null;
      setSending({ detail: null });
      const out = await send(app, { projectId, draftTaskId: NEW_CONVERSATION, text, annotationId: null, label, build: () => startConversation(projectId, words, chosenAgent, { access }) }, (detail) => setSending({ detail }));
      setSending(null);
      if (out.ok) sent();
      else announce(out.attempt.detail, "assertive");
      return;
    }
    if (intent.kind === "new_task" && taskId !== null) {
      moveToNewTask();
      ui.push({ screen: "review", projectId, taskId: null, intent: "new_task" });
      return;
    }
    if (review) {
      ui.push({ screen: "review", projectId, taskId, intent: reviewIntent(intent) });
      return;
    }
    if (!task) return;
    const body = text.trim();
    const req: SendRequest =
      intent.kind === "continue"
        ? { projectId, draftTaskId: taskId, text, annotationId: null, label, expectedRevision: task.revision, build: () => ({ command: "resume_task", task_id: task.task_id, text: body, ...(task.conversation ? turnModel(task, harness ?? undefined, draft?.agent) : {}) }) }
        : intent.kind === "continue_later"
        ? { projectId, draftTaskId: taskId, text, annotationId: null, label: "Put in Next up", expectedRevision: task.revision, build: () => ({ command: "queue_continuation", task_id: task.task_id, text: body, authorization: "auto_start" }) }
        : {
            projectId,
            draftTaskId: taskId,
            text,
            annotationId: null,
            label: option?.label ?? "Send",
            build: (bundle) => ({
              command: "send_message",
              task_id: task.task_id,
              text: body,
              bundle,
              delivery: intent.kind === "message" ? intent.delivery : "next_turn",
            }),
          };
    setSending({ detail: null });
    const out = await send(app, req, (detail) => setSending({ detail }));
    setSending(null);
    if (out.ok) sent();
    else announce(out.attempt.detail, "assertive");
  };

  /** Ask the task's own agent, read-only, what happened. The field is left as it is. */
  const askSummary = async (anyway = false) => {
    setToolsOpen(false);
    if (!task || !summary || summaryBusy || (unresolved && !anyway)) return;
    setSending({ detail: null });
    const out = await send(app, task.conversation ? conversationSummary(projectId, task) : summaryRequest(projectId, task), (detail) => setSending({ detail }));
    setSending(null);
    if (out.ok) sent();
    else announce(out.attempt.detail, "assertive");
  };

  /** Carry the conversation on as a task in Work, in the words the sheet holds. */
  const carry = async (words: string) => {
    if (!task || carryBlocked) return;
    setSending({ detail: null });
    const out = await send(app, intoTask(projectId, task, words), (detail) => setSending({ detail }));
    setSending(null);
    setCarrying(null);
    if (out.ok) sent();
    else announce(out.attempt.detail, "assertive");
  };

  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void go();
    }
  };

  /** Opens this message's drawing, started at what was chosen under Attach. */
  const attach = (start?: SourceStart) => {
    let id = draft?.annotationId ?? null;
    if (!id || !store.get()[id]) {
      id = store.create(projectId, taskId).id;
      ui.saveDraft(projectId, taskId, { annotationId: id });
    }
    if (start) ui.requestSource(id, start);
    ui.push({ screen: "annotate", annotationId: id, intent: reviewIntent(intent) });
  };

  const detach = () => {
    if (draft?.annotationId) removeMarkup(ui, store, projectId, taskId, draft.annotationId);
  };

  const dismiss = () => ui.saveDraft(projectId, taskId, { attempt: null });
  const reviewContinuation = () => {
    // A message that never left has nothing to settle; one that may have
    // arrived stays, so Review still asks before sending anyway.
    if (attempt?.phase !== "unknown") dismiss();
    ui.push({ screen: "review", projectId, taskId, intent: "continue" });
  };
  const again = async () => {
    if (!attempt || retryRecipient.blocked) return;
    setSending({ detail: null });
    let out;
    try {
      out = await resend(app, projectId, taskId, attempt, "Send again");
    } catch (e) {
      // Refused before anything left: the draft already says why.
      setSending(null);
      announce((e as Error).message, "assertive");
      return;
    }
    setSending(null);
    if (out.ok) sent();
    else announce(out.attempt.detail, "assertive");
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

  return (
    <div className={`composer${hasContent ? " is-typing" : ""}`} aria-label="Message">
      <SavedRetryDrafts projectId={projectId} taskId={taskId} />
      {/* Unless it can wait in Next up, nothing typed here could go anywhere, so only the block shows. */}
      {blockedByWriter && writer && <div className={`composer__blocker${waitsInNextUp ? " is-queued" : ""}`} role="status">
        <p>{waitsInNextUp ? writerQueueLine(writer) : writerBlockLine(writer)}</p>
        <span className="composer__blocker-actions">
          {summary && !waitsInNextUp && <Button size="sm" variant="quiet" icon={ScrollText} pending={sending !== null} disabled={summaryBusy || unresolved} onClick={() => void askSummary()}>Summarize</Button>}
          <Button size="sm" variant="secondary" aria-label={writer.unresolved ? "Check how the blocking task ended" : "Open the blocking task"} onClick={() => {
            if (writer.unresolved) ui.push({ screen: "receipt", taskId: blockedByWriter.task_id });
            else ui.openTask(blockedByWriter.project_id, blockedByWriter.task_id);
          }}>{writer.unresolved ? "Check" : "Open"}</Button>
        </span>
      </div>}
      {canRepeat && <TaskRecipient recipient={retryRecipient} retry />}
      {attempt && !sending && (
        <AttemptNotice body={attempt?.body}
          phase={attempt.phase}
          detail={attempt.detail}
          checking={checking}
          canCheck={online && attempt.commandId !== null}
          repeatable={canRepeat}
          retryBlocked={canRepeat ? retryRecipient.blocked : attempt.readOnly ? !summary || summaryBusy : attempt.asTask ? false : recipientBlocked}
          reviews={!canRepeat && !attempt.readOnly && !attempt.asTask && (review || overtaken)}
          ended={overtaken}
          asked={attempt.readOnly}
          onCheck={() => void recheck()}
          onRetry={() => void (canRepeat ? again() : attempt.readOnly ? askSummary(true) : attempt.asTask ? setCarrying(attempt.text) : overtaken ? reviewContinuation() : go(true))}
          onDismiss={dismiss}
        />
      )}
      {mode.mode === "waiting" && (
        <div className={`composer__queue${queued && queue?.paused ? " is-paused" : ""}`}>
          <details>
            <summary>{queued ? queue?.paused ? "Next up is paused" : queueItemState(queued, Math.max(0, pendingQueueItems(queue).indexOf(queued))) : queue ? "Not queued" : "Queue unknown"}</summary>
            <p>{ready ? "Nothing starts until you say so. Review its prompt and start it." : queued ? "This task is waiting in Next up. Review its start permission there." : queue ? "This task has no pending queue item. It won’t start from Next up." : "Connect to check whether this task is still in Next up."} Anything you write here becomes a separate task.</p>
          </details>
          {queued && queue?.paused && <p className="composer__queue-reason">{queue.pause_reason ?? "Automatic starts are paused."}</p>}
          {/* The task waits on the person: the way on is the button, not a link. */}
          <Button size="sm" variant={ready ? "primary" : "quiet"} onClick={() => { if (queued) ui.push({ screen: "review", projectId, taskId, queueItemId: queued.item_id }); else ui.push({ screen: "next_up" }, "work"); }}>{ready ? "Review and start" : queued ? "Review queued task" : "Next up"}</Button>
        </div>
      )}
      {mode.mode === "reconcile" && (
        <Notice
          tone="warn"
          title="Outcome unknown"
          action={
            <Button size="sm" variant="secondary" trailingIcon={ChevronRight} onClick={() => task && ui.push({ screen: "receipt", taskId: task.task_id })}>
              What happened
            </Button>
          }
        >
          {profile.name} couldn’t confirm how the last run ended. Look at what happened before starting more work. Anything you write here becomes a separate task.
        </Notice>
      )}
      {mode.mode === "archived" && (
        <Notice tone="info" title="Archived">
          This task is kept for the record. Anything you write here becomes a separate task.
        </Notice>
      )}

      {!online && <details className="composer__connection">
        <summary>{link.state === "failed" ? "Access unavailable" : "Offline"}<span className="composer__connection-more"> · Details</span></summary>
        <p>{disabledReason ?? `${profile.name} isn’t connected. What you write stays on this device.`}</p>
      </details>}
      {(!blockedByWriter || waitsInNextUp) && <>
        {intent.kind !== "new_task" && intent.kind !== "next_up" && <TaskRecipient recipient={taskRecipient} onNewTask={unresolved || attempt?.phase === "sending" || sending ? undefined : () => {
          moveToNewTask();
          ui.push({ screen: "review", projectId, taskId: null, intent: "new_task" });
        }} />}
        <div className="composer__chips">
          {mode.mode === "new" && agent && (
            <Chip icon={Bot} className="composer__chip composer__opt composer__extra" onClick={() => setChoosingAgent(true)} aria-haspopup="dialog">
              {agentLabel(harnesses, agent)}
              <ChevronDown className="composer__chip-chev" aria-hidden />
            </Chip>
          )}
          {mode.mode === "new" && !fresh && launchers && canWork && (draft?.review?.start ? (
            <span className="composer__attach">
              <button type="button" className="composer__attach-open" onClick={() => setChoosingStart(true)} aria-haspopup="dialog">
                {draft.review.start.start.type === "scratch" ? <NotebookPen aria-hidden strokeWidth={2.1} /> : draft.review.start.start.type === "agent" ? <Bot aria-hidden strokeWidth={2.1} /> : <Sparkles aria-hidden strokeWidth={2.1} />}
                {startedLabel(draft.review.start)}
              </button>
              <button type="button" className="composer__attach-x" aria-label={`Don’t start from ${draft.review.start.name}`} onClick={() => setDraftStart(ui, projectId, null)}>
                <X aria-hidden strokeWidth={2.2} />
              </button>
            </span>
          ) : (
            <Chip icon={Sparkles} className="composer__chip composer__opt composer__extra" onClick={() => setChoosingStart(true)} aria-haspopup="dialog">
              Start from
              <ChevronDown className="composer__chip-chev" aria-hidden />
            </Chip>
          ))}
          {mode.mode === "new" && !fresh && draft?.review?.github?.type === "new_pr" && (
            <span className="composer__attach">
              <span className="composer__attach-open"><GitPullRequest aria-hidden strokeWidth={2.1} />Pull request</span>
              <button type="button" className="composer__attach-x" aria-label="Don’t open a pull request"
                onClick={() => ui.saveDraft(projectId, null, { review: { ...draft.review!, github: null } })}>
                <X aria-hidden strokeWidth={2.2} />
              </button>
            </span>
          )}
          {task && next && (
            <Chip icon={Bot} className="composer__chip composer__opt" onClick={() => setChoosingModel(true)} aria-haspopup="dialog">
              <span className="visually-hidden">Model for your next message:</span>{" "}{agentLabel(harnesses, next)}
              <ChevronDown className="composer__chip-chev" aria-hidden />
            </Chip>
          )}
          {carryable && (
            <Chip icon={Hammer} className="composer__chip composer__opt" onClick={() => setCarrying(text.trim() || INTO_TASK_TEXT)} aria-haspopup="dialog">
              Turn into a task
            </Chip>
          )}
          {options.length > (talking ? 1 : 0) && (
            <Chip className={`composer__chip composer__how${label === "Stop and send" ? " composer__how--warn" : " composer__opt"}`} onClick={() => setPicking(true)} aria-haspopup="dialog">
              {intent.kind === "message" && <><span className="visually-hidden">How to send:</span>{" "}</>}{option?.label ?? "Send"}
              <ChevronDown className="composer__chip-chev" aria-hidden />
            </Chip>
          )}
          {taskAgent && <TaskAgentChip agent={taskAgent} onClick={() => setShowingAgent(true)} />}
          {summary && !talking && (
            <Chip icon={ScrollText} className="composer__chip composer__opt composer__summary" disabled={summaryBusy || unresolved} onClick={() => void askSummary()}>
              Summarize
            </Chip>
          )}
          {(readOnly || (intent.kind === "new_task" && mode.mode !== "new")) && (
            <span className="composer__as">
              <ListPlus aria-hidden strokeWidth={2.1} />
              New task in {project.name}
            </span>
          )}
          {annotation && (
            <span className="composer__attach">
              <button type="button" className="composer__attach-open" onClick={() => attach()}>
                <Paperclip aria-hidden strokeWidth={2.1} />
                {describe(summarize(annotation))}
              </button>
              <button type="button" className="composer__attach-x" aria-label="Remove the markup" onClick={detach}>
                <X aria-hidden strokeWidth={2.2} />
              </button>
            </span>
          )}
        </div>

        <div className="composer__row">
          {/* One bar, as in a chat app: the tools at its start, then the words,
              and at its end Attach while the field is empty or Send
              once there is something to send. The words keep the width
              between, whatever the end shows. */}
          <div className={`composer__bubble${hasContent ? " has-content" : ""}`}>
            <IconButton icon={SlidersHorizontal} label="Message tools" className="composer__tools" aria-haspopup="dialog" onClick={() => setToolsOpen(true)} />
            <TextArea
              ref={field}
              className="composer__field"
              label={fresh ? `Ask ${agentName} anything about ${project.name}` : talking && intent.kind === "continue" ? `Reply to ${agentName}` : placeholder(intent, agentName, project.name)}
              hideLabel
              placeholder={fresh ? (tight ? "Message" : `Message ${agentName}`) : talking && intent.kind === "continue" ? (tight ? "Reply" : `Reply to ${agentName}`) : shortPlaceholder(intent, agentName, tight, !!task && readOnly)}
              value={text}
              onChange={setText}
              onKeyDown={onKey}
              // On a cramped screen a longer draft scrolls inside one row.
              maxRows={cramped ? 1 : tight ? 2 : 6}
              enterKeyHint="enter"
              autoCapitalize="sentences"
              spellCheck
            />
            {!fresh && <IconButton icon={Paperclip} label="Attach" className="composer__pic" aria-haspopup="dialog" onClick={() => setAttaching(true)} {...(hasContent ? away : {})} />}
            {/* One icon for every way of sending; its name says which, and
                Review only opens the review, so nothing goes unseen. */}
            <IconButton
              icon={SendHorizontal}
              variant="primary"
              label={sending ? `Sending to ${profile.name}` : label}
              className={`composer__send${label === "Stop and send" ? " composer__send--warn" : ""}${sending ? " is-pending" : ""}`}
              aria-busy={sending !== null || undefined}
              disabled={!hasContent || disabledReason !== null || unresolvedReason !== null || sending !== null || attempt?.phase === "sending"}
              onClick={() => void go()}
              {...(hasContent ? {} : away)}
            />
          </div>
        </div>
        {controlReason && (unlockable ? <Unlock />
          : <Button onClick={() => ui.push({ screen: "control" }, "control")}>Open Control</Button>)}
        {(sending || (online && (disabledReason || unresolvedReason))) && (
          <p ref={readinessHint} className="composer__hint" role="status">
            {sending
              ? (sending.detail ?? `Sending to ${profile.name}`)
              : (disabledReason ?? unresolvedReason ?? `${profile.name} isn’t connected. What you write stays on this device.`)}
          </p>
        )}
      </>}

      {planError && <Notice tone="warn">{planError}</Notice>}
      <Sheet open={toolsOpen} onClose={() => setToolsOpen(false)} title="Message tools">
        <div className="group">
          {!fresh && <ListRow icon={Paperclip} title="Attach" subtitle="Screen, images, a clip or markup" chevron onClick={() => { setToolsOpen(false); setAttaching(true); }} />}
          {mode.mode === "new" && agent && <ListRow icon={Bot} title="Agent, model and effort" subtitle={agentLabel(harnesses, agent)} chevron onClick={() => { setToolsOpen(false); setChoosingAgent(true); }} />}
          {next && <ListRow icon={Bot} title="Model and effort" subtitle={`${agentLabel(harnesses, next)} · From your next message`} chevron onClick={() => { setToolsOpen(false); setChoosingModel(true); }} />}
          {mode.mode === "new" && !fresh && launchers && canWork && <ListRow icon={Sparkles} title="Start from" subtitle={startedLabel(draft?.review?.start) ?? undefined} chevron onClick={() => { setToolsOpen(false); setChoosingStart(true); }} />}
          {taskAgent && <ListRow icon={Bot} title="Agent, model and effort" subtitle={taskAgent.label} chevron onClick={() => { setToolsOpen(false); setShowingAgent(true); }} />}
          {carryPlan && <ListRow icon={ListChecks} title="Carry out this plan" subtitle="Review a new run in Work mode" chevron onClick={() => carryPlan()} />}
          {summary && <ListRow icon={ScrollText} title={talking ? "Sum up so far" : "Summarize what happened"}
            subtitle={talking ? `Asks ${agentName} for what was discussed and decided, and what’s still open.` : `Asks ${taskAgent?.label ?? agentName} in this task’s session. It can read but not change anything.`} chevron
            onClick={summaryBusy || unresolved ? undefined : () => void askSummary()} className={summaryBusy || unresolved ? "talk-agents__off" : undefined} />}
          {carryable && <ListRow icon={Hammer} title="Turn into a task" subtitle={`${agentName} carries on in Work mode and can change files`} chevron
            onClick={() => { setToolsOpen(false); setCarrying(text.trim() || INTO_TASK_TEXT); }} />}
          {options.length > (talking ? 1 : 0) && <ListRow icon={SendHorizontal} title="How to send" subtitle={option?.label} chevron onClick={() => { setToolsOpen(false); setPicking(true); }} />}
        </div>
      </Sheet>
      {!fresh && (
        <AttachSheet
          open={attaching}
          onClose={() => setAttaching(false)}
          online={online}
          computer={profile.name}
          markup={annotation && describe(summarize(annotation))}
          onStart={(start) => {
            setAttaching(false);
            attach(start);
          }}
        />
      )}
      {options.length > 0 && (
        <IntentSheet
          open={picking}
          onClose={() => setPicking(false)}
          options={options}
          value={intent}
          onCarryPlan={carryPlan && (() => carryPlan())}
          onPick={(i) => {
            setChosen(i);
            setPicking(false);
            field.current?.focus();
          }}
        />
      )}
      {taskAgent && <TaskAgentSheet open={showingAgent} onClose={() => setShowingAgent(false)} agent={taskAgent}
        onNewTask={unresolved || attempt?.phase === "sending" || sending ? undefined : moveToNewTask} />}
      {mode.mode === "new" && agent && (
        <AgentSheet
          open={choosingAgent}
          onClose={() => setChoosingAgent(false)}
          project={project}
          value={agent}
          onChange={(a) => ui.saveDraft(projectId, taskId, { agent: a })}
          {...(fresh ? { mode: "ask" as const, launchFlags: false, title: "Agent for this conversation", doneLabel: "Chat with this agent" } : {})}
        />
      )}
      {task && next && (
        <AgentSheet
          open={choosingModel}
          onClose={() => setChoosingModel(false)}
          project={project}
          value={next}
          onChange={(a) => ui.saveDraft(projectId, taskId, { agent: { harness: a.harness, model: a.model, effort: a.effort } })}
          mode="ask"
          launchFlags={false}
          fixedAgent
          title="Model for your next message"
          doneLabel="Use from the next message"
          note={mode.mode === "running" ? "This reply finishes as it started. Your next message after it uses this." : "Your next message uses this, and later ones keep it until you change it."}
        />
      )}
      {task && carryable && (
        <IntoTaskSheet
          open={carrying !== null}
          onClose={() => setCarrying(null)}
          project={project}
          agent={agentName}
          access={task.access ?? null}
          initial={carrying ?? ""}
          blocked={carryBlocked}
          onOpenBlocker={writer?.task && writer.task.task_id !== task.task_id ? () => {
            const held = writer.task!;
            setCarrying(null);
            if (writer.unresolved) ui.push({ screen: "receipt", taskId: held.task_id });
            else ui.openTask(held.project_id, held.task_id);
          } : undefined}
          pending={sending !== null}
          onStart={(words) => void carry(words)}
        />
      )}
      {mode.mode === "new" && !fresh && launchers && canWork && (
        <StartFromSheet
          open={choosingStart}
          onClose={() => setChoosingStart(false)}
          projectId={projectId}
          onChoose={(start, a) => { setChoosingStart(false); setDraftStart(ui, projectId, start, a); }}
        />
      )}
    </div>
  );
}

export function AttemptNotice({
  body,
  phase,
  detail,
  checking,
  canCheck,
  onCheck,
  onRetry,
  onDismiss,
  repeatable = false,
  retryBlocked = false,
  reviews = false,
  ended = false,
  asked = false,
}: {
  body?: SendAttempt["body"];
  phase: SendAttempt["phase"];
  detail: string;
  /** Sending again reuses the original id, so the computer can't act twice. */
  repeatable?: boolean;
  retryBlocked?: boolean;
  /** The retry opens Review rather than sending, so its label says so. */
  reviews?: boolean;
  /** The message's run is over, so sending it now starts a new run. */
  ended?: boolean;
  /** A question the app wrote, such as Summarize, rather than the person's text. */
  asked?: boolean;
  checking: boolean;
  canCheck: boolean;
  onCheck: () => void;
  onRetry: () => void;
  onDismiss: () => void;
}) {
  const { profile } = useApp();
  const online = useLink().state === "online";
  const close = <IconButton icon={X} label="Dismiss" className="composer__notice-x" onClick={onDismiss} />;
  const over = ended && " This task has finished; sending starts a new run.";
  const report = phase === "sending" ? false : "always";
  switch (phase) {
    case "sending":
      return (
        <Notice report={report} reportDetails={{ command: body?.command, error: { code: `send_${phase}`, message: detail } }} tone="info" title="Sending" className="composer__notice">
          {detail}
        </Notice>
      );
    case "unknown":
      return (
        <Notice report={report} reportDetails={{ command: body?.command, error: { code: `send_${phase}`, message: detail } }}
          tone="unknown"
          title="It may have arrived"
          className="composer__notice"
          action={
            <span className="composer__notice-actions">
              <Button size="sm" variant="secondary" pending={checking} disabled={!canCheck} onClick={onCheck}>
                Check
              </Button>
              <Button size="sm" variant="quiet" disabled={retryBlocked} onClick={onRetry}>
                {repeatable ? "Send again" : reviews ? "Review and send anyway" : "Send anyway"}
              </Button>
            </span>
          }
        >
          {detail}
          {repeatable && " Sending again repeats the same request, so it can’t arrive twice."}
          {over}
        </Notice>
      );
    case "not_delivered":
      // Nothing left the phone, so once the computer is back nothing is in doubt.
      return (
        <Notice report={report} reportDetails={{ command: body?.command, error: { code: `send_${phase}`, message: detail } }}
          tone={online ? "info" : "warn"}
          title={online ? "Not sent yet" : "Not sent"}
          className="composer__notice"
          action={
            <span className="composer__notice-actions">
              <Button size="sm" variant="secondary" disabled={retryBlocked} onClick={onRetry}>
                {reviews ? "Review and send again" : "Send again"}
              </Button>
              {close}
            </span>
          }
        >
          {detail}
          {online && ` ${profile.name} is connected now.`}
          {over}
        </Notice>
      );
    case "rejected":
      return (
        <Notice report={report} reportDetails={{ command: body?.command, error: { code: `send_${phase}`, message: detail } }} tone="stop" title="Not sent" className="composer__notice" action={close}>
          {detail}{asked ? "" : " Your text is still here."}{over}
        </Notice>
      );
    case "failed":
      return (
        <Notice report={report} reportDetails={{ command: body?.command, error: { code: `send_${phase}`, message: detail } }}
          tone="stop"
          title="Couldn’t send"
          className="composer__notice"
          action={
            <span className="composer__notice-actions">
              <Button size="sm" variant="secondary" disabled={retryBlocked} onClick={onRetry}>
                {reviews ? "Review and send again" : "Try again"}
              </Button>
              {close}
            </span>
          }
        >
          {detail}
          {over}
        </Notice>
      );
  }
}

function IntentSheet({
  open,
  onClose,
  options,
  value,
  onPick,
  onCarryPlan,
}: {
  open: boolean;
  onClose: () => void;
  options: IntentOption[];
  value: Intent;
  onPick: (i: Intent) => void;
  onCarryPlan?: () => void;
}) {
  return (
    <Sheet open={open} onClose={onClose} title="How to send">
      {onCarryPlan && (
        <div className="group composer__carry">
          <ListRow title="Carry out this plan" subtitle="Review Work mode and permissions before starting" chevron onClick={onCarryPlan} />
        </div>
      )}
      <div className="group" role="radiogroup" aria-label="How to send">
        {options.map((o) => (
          <ListRow
            key={o.label}
            title={o.label}
            subtitle={o.unavailable ?? o.detail}
            checked={sameIntent(o.intent, value)}
            className={o.unavailable ? "talk-agents__off" : undefined}
            onClick={o.unavailable ? undefined : () => onPick(o.intent)}
          />
        ))}
      </div>
    </Sheet>
  );
}
