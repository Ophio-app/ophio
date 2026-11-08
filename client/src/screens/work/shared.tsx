// SPDX-License-Identifier: Apache-2.0
import { unsentAnswers } from "../talk/previewAnswers";
import { HandBack } from "../talk/HandBack";
import { handBackReason, handBackTarget } from "../../state/handBack";
import { besideBadge, taskActivity, taskAttentionReason, taskStatus } from "./taskActivity";
import {
  Archive,
  CircleCheck,
  CircleHelp,
  CircleX,
  Clock,
  MessageCircleQuestion,
  MessagesSquare,
  ShieldAlert,
} from "lucide-react";
import { useState, type ReactNode } from "react";
import { useHandledWhenSeen } from "../../platform/handled";
import { useOnScreen } from "../../shell/Screen";
import {
  describeError,
  shallowEqual,
  useApp,
  useHost,
  useLink,
} from "../../app/context";
import { useLastHeard } from "../../app/lastHeard";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { ApprovalQuickActions } from "../talk/ApprovalQuickActions";
import { listedEverywhere } from "../../app/fleet";
import { useApprovalWayButtons } from "../talk/ApprovalWayButtons";
import { uuidv7 } from "../../protocol/ids";
import type { HostSession, PendingCommand } from "../../host/session";
import { commandResult } from "../../host/commandResult";
import { confirmHere } from "../settings/useAccounts";
import { ApprovalCard, ApprovalDetailText, FactsLine } from "../../design/ApprovalCard";
import { ListRow } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { NotAllowed } from "../../shell/NotAllowed";
import { Sheet } from "../../design/Sheet";
import { StateLine } from "../../design/StateLine";
import { announce } from "../../design/Announcer";
import { useStore } from "../../state/store";
import { isActiveTask, openAttention, reviewAttention, type HostState } from "../../state/projection";
import type {
  Approval,
  ApprovalDecision,
  AttentionItem,
  Command,
  Task,
} from "../../protocol/types";
import { StopTask as NamedStopTask, StopStatus, useStopIntent } from "../control/StopTask";
import { attentionGroups, resultLabel } from "./attention";
import { useLocalText } from "./local";
import { harnessLabel, latestRun, modelLabel } from "./logic";
import { startedLabel } from "../../app/launchers";
import { taskRowBadge } from "../talk/TaskBar";
import { useMayDo } from "../talk/MayDoLine";
import { mayDo } from "../talk/mayDo";
import { accessHeld } from "../talk/access";

/** Tasks the computer has from before those this device holds, read a page
 * at a time when the person reaches the end of a list. */
export function EarlierTasks({ projectId }: { projectId: string }) {
  const { session, profile } = useApp();
  const link = useLink();
  const more = useHost((s) => projectId in s.olderTasks);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!more) return null;
  const load = async () => {
    setBusy(true);
    setError(null);
    try {
      await session.loadOlderTasks(projectId);
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      {error && <Notice tone="stop">{error}</Notice>}
      <Button variant="secondary" disabled={link.state !== "online" || busy} pending={busy} onClick={() => void load()}>
        Show earlier tasks
      </Button>
    </>
  );
}
export function ConnectionNote() {
  const link = useLink();
  const lastHeard = useLastHeard(link.state !== "online");
  const heard = link.state === "online" ? null : lastHeard;
  if (link.state === "online") return null;
  return (
    <Notice
      tone={link.state === "failed" ? "stop" : "unknown"}
      title={
        link.state === "failed"
          ? "Access unavailable"
          : link.state === "offline"
            ? link.noTailscale ? "Tailscale off" : "Offline"
            : "Connecting…"
      }
    >
      {link.state === "failed"
        ? link.detail
        : link.state === "offline" && link.noTailscale
          ? link.reason
          : "The computer hasn’t confirmed any newer changes."}
      {heard !== null && (
        <>
          {" "}
          Last heard <Age at={heard} suffix=" ago" />.
        </>
      )}
    </Notice>
  );
}
export function useCanWork() {
  const link = useLink();
  const grant = useHost((s) => s.grants?.tasks);
  return link.state === "online" && grant === true;
}
export function TaskPermissionNote() {
  const granted = useHost((s) => s.grants?.tasks);
  return granted === false ? (
    <NotAllowed
      what="start, steer or stop tasks"
      permission="tasks"
      detail="It still shows the work the computer last reported."
    />
  ) : null;
}
export function useOpenTask() {
  const { ui } = useApp();
  return (task: Task) => {
    // A conversation is its chat, whatever its state; a task's receipt reads better once it ends.
    if (isActiveTask(task) || task.conversation) {
      ui.openTask(task.project_id, task.task_id);
    } else ui.push({ screen: "receipt", taskId: task.task_id });
  };
}
export function taskIcon(task: Task, reason: string | null) {
  return task.conversation && !reason
    ? MessagesSquare
    : task.state === "archived"
    ? Archive
    : task.state === "completed"
    ? CircleCheck
    : task.state === "failed"
      ? CircleX
      : task.state === "reconciliation_required"
        ? CircleHelp
        : task.state === "awaiting_decision" || (task.state === "working" && reason)
          ? MessageCircleQuestion
          : Clock;
}
/** Where an item is, in All's lists: its computer, isolated so no name can
 * turn the text beside it around, and its project. */
export function WhereLine({ projectId }: { projectId: string | undefined }) {
  const { profile } = useApp();
  const project = useHost((s) => (projectId ? s.projects[projectId]?.name : undefined));
  return <span className="work-row-line work-row-where"><bdi className="all-projects__computer">{profile.name}</bdi>{project ? <> · {project}</> : null}</span>;
}

/** A task in a list. `where` names its computer and project, as All does. */
export function TaskRow({ task, where = false }: { task: Task; where?: boolean }) {
  const stop = useStopIntent(task);
  const attentionCount = useHost(s => openAttention(s).filter(a => a.task_id === task.task_id).length);
  const run = useHost((s) => latestRun(task, s.runs));
  const queue = useHost((s) => s.queues[task.project_id]);
  const harnesses = useHost((s) => s.harnesses);
  const status = useHost(s => taskStatus(task, run, s.control[task.target_id ?? ""]));
  const reason = useHost(s => taskAttentionReason(s, task));
  const activity = useHost((s) => reason ? undefined : taskActivity(s, task));
  const input = useHost((s) =>
    task.target_id ? s.control[task.target_id] : undefined,
  );
  const open = useOpenTask();
  const badge = stop?.phase === "not_running" ? "Not running · result being checked" : taskRowBadge(task, run, queue, reason).label;
  const line = besideBadge(reason ?? activity?.text ?? status, badge);
  // Whether it is a chat or a task, and what it may do; the row opens it.
  const may = useMayDo(task);
  return (
    <ListRow
      icon={taskIcon(task, reason)}
      title={task.title}
      chevron
      onClick={() => open(task)}
      subtitle={
        <>
          <span className="work-row-state">
            {badge} · {modelLabel(run, harnesses, task)}{task.start ? ` · ${startedLabel(task.start)}` : ""} ·{" "}
            <Age at={activity?.at ?? task.updated_at} suffix=" ago" />
          </span>
          {where && <WhereLine projectId={task.project_id} />}
          <span className="work-row-may">{may}</span>
          <StopStatus task={task} />
          {line && <span className="work-row-line">{line}</span>}
          {input?.input_state === "agent" && (
            <span className="work-row-line">Agent is in control of the screen</span>
          )}
          {attentionCount > 0 && (
            <span className="work-row-line">
              {attentionCount} {attentionCount === 1 ? "needs" : "need"} attention
            </span>
          )}
        </>
      }
    />
  );
}
/** A command is in doubt while it is on its way, or while its outcome can
 * still be learned. A computer that restarted can't say what it received
 * before; what it reports now is what the person acts on. */
export function inDoubt(session: HostSession, online: boolean, c: PendingCommand): boolean {
  return c.phase === "sending" || c.error?.code === "outcome_unknown"
    || (c.phase === "unknown" && (!online || session.sameCommandHistory(c.historyId)));
}
/** The computer refused this command, so it did nothing. */
const refused = (c: PendingCommand) => c.phase === "rejected" && c.error?.code !== "outcome_unknown";
/** `runEnded`: the run these commands were for ended before any was recorded
 * as taking effect. One the computer refused, or failed to hand on, was not
 * applied; one its agent took as the run ended may have been. */
export function CommandFeedback({ match, limit = 3, hideAccepted = false, runEnded = false }: { match: (c: Command, pending: PendingCommand) => boolean; limit?: number; hideAccepted?: boolean; runEnded?: boolean }) {
  const { session } = useApp();
  const commands = useStore(
    session.commands,
    (all) =>
      Object.values(all)
        .filter((c) => match(c.body, c) && (!hideAccepted || (c.phase !== "accepted" && c.phase !== "duplicate")))
        .sort((a, b) => b.issuedAt - a.issuedAt)
        .slice(0, limit),
    shallowEqual,
  );
  return (
    <div className="work-feedback">
      {commands.map((c) => <CommandLine key={c.id} command={c} runEnded={runEnded} />)}
    </div>
  );
}
function CommandLine({ command: c, runEnded }: { command: PendingCommand; runEnded: boolean }) {
  const { session, profile } = useApp();
  const online = useLink().state === "online";
  const effect = useHost((s) => s.commandStages[c.id]);
  const late = runEnded && (refused(c) || (effect?.stage === "failed" && effect.error?.code !== "outcome_unknown"));
  const overtaken = runEnded && !late && (c.phase === "accepted" || c.phase === "duplicate");
  return (
    <div>
      <p className="work-caption">{c.label}</p>
      <StateLine
        phase={
          late
            ? "arrived_late"
            : overtaken || c.phase === "unknown"
            ? "unknown"
            : c.phase === "accepted" || c.phase === "duplicate"
              ? "host_accepted"
              : c.phase === "rejected"
                ? c.error?.code === "outcome_unknown"
                  ? "unknown"
                  : "rejected"
                : c.phase === "not_delivered"
                  ? "not_delivered"
                  : "sending"
        }
        reportDetails={{ command: c.body.command, error: effect?.error ?? c.error ?? { code: c.phase } }}
        computer={profile.name}
        detail={
          late
            ? null
            : overtaken
            ? effect?.error?.message ?? "The run ended as this arrived; check what the agent did"
            : c.phase === "unknown"
            ? inDoubt(session, online, c)
              ? "The computer hasn’t confirmed the result. Reconnect to reconcile it."
              : `${profile.name} restarted, so it can’t say whether this happened. Check the result before doing it again.`
            : c.error?.message
        }
      />
    </div>
  );
}
/** Unknown effects must reconcile before the person can repeat an action. */
export function useUnconfirmedCommand(match: (command: Command, pending: PendingCommand) => boolean) {
  const { session } = useApp();
  const online = useLink().state === "online";
  return useStore(session.commands, (all) =>
    Object.values(all).some((c) => match(c.body, c) && inDoubt(session, online, c)),
  );
}
export function StopTask({ task, describedBy }: { task: Task; label?: string; describedBy?: string }) {
  return <NamedStopTask task={task} describedBy={describedBy} />;
}
/** A project's open items, or its results not yet looked at, approvals
 * first, as the attention list shows them. */
/** What a project, or with `null` every project, lists under Needs you, or
 * with `review` under To look at. */
export function listedAttention(s: HostState, projectId: string | null, review = false): AttentionItem[] {
  return (projectId === null ? listedEverywhere(s, review) : (review ? reviewAttention(s) : openAttention(s))
    .filter((a) => !!a.task_id && s.tasks[a.task_id]?.project_id === projectId))
    .sort((a, b) => Number(!!b.approval_id) - Number(!!a.approval_id) || b.priority - a.priority);
}

/** `lead` sets the first of the list's groups apart, outlined, as the one to
 * answer first; `quiet` says nothing when the list is empty, for a list that
 * says it once elsewhere. */
export function AttentionList({ projectId, review = false, lead = false, quiet = false, where = false }: { projectId: string | null; review?: boolean; lead?: boolean; quiet?: boolean; where?: boolean }) {
  const { session, ui } = useApp();
  const attention = useHost((s) => listedAttention(s, projectId, review), shallowEqual);
  const approvalDetails = useHost(s => s.approvals);
  const results = useHost(s => Object.fromEntries(attention.filter(a => a.kind === "completed" || a.kind === "failure").map(a => [a.attention_id, resultLabel(s, a)])), shallowEqual);
  // Whether each is a chat or a task, and what it may do: a chat waiting
  // here is still a chat.
  const mayLines = useHost(s => {
    const held = accessHeld(s.capabilities);
    return Object.fromEntries(attention.flatMap(a => {
      const t = a.task_id ? s.tasks[a.task_id] : undefined;
      return t ? [[a.attention_id, mayDo(t, held, s.projects)]] : [];
    }));
  }, shallowEqual);
  const [chosen, setChosen] = useState<string | null>(null);
  const [handBackId, setHandBackId] = useState<string | null>(null);
  const handBackTask = useHost(s => handBackId ? s.tasks[handBackId] : undefined);
  const approval = useHost((s) => (chosen ? s.approvals[chosen] : undefined));
  const approvalsGranted = useHost((s) => s.grants?.approvals !== false);
  const task = useHost((s) =>
    approval ? s.tasks[approval.task_id] : undefined,
  );
  const open = useOpenTask();
  const groups = attentionGroups(attention);
  const shown = (group: AttentionItem[]) => {
    const a = group[0];
    if (!a) return null;
    const state = session.state.get();
    const waitingTask = a.task_id ? state.tasks[a.task_id] : undefined;
    const queueGoAhead = a.kind === "waiting_for_you" && waitingTask?.state === "accepted" && !a.run_id;
    const waitingTarget = waitingTask && a.kind === "waiting_for_you" ? handBackTarget(state, waitingTask) : undefined;
    if (group.length > 1) return <details className="work-attention-group" key={a.attention_id}>
      <summary>{group.length} {a.kind === "approval" ? "approvals" : "requests"} · {a.task_id ? session.state.get().tasks[a.task_id]?.title : a.title}
        {where && <WhereLine projectId={waitingTask?.project_id} />}
        {mayLines[a.attention_id] && <span className="work-row-may">{mayLines[a.attention_id]}</span>}</summary>
      {group.map(item => {
        const asked = item.approval_id ? approvalDetails[item.approval_id] : undefined;
        return <SeenRow key={item.attention_id} ids={[item.attention_id]}><ListRow title={asked?.headline || item.title}
          subtitle={asked ? (asked.facts?.length ? <FactsLine facts={asked.facts} /> : <ApprovalDetailText approval={asked} brief />) : item.body}
          chevron onClick={() => item.approval_id ? setChosen(item.approval_id) : ui.push({ screen: "attention", attentionId: item.attention_id })} /></SeenRow>;
      })}
    </details>;
    return <SeenRow key={a.attention_id} ids={[a.attention_id]} className="work-attention-item">
    <ListRow
      icon={
        a.approval_id
          ? ShieldAlert
          : a.kind === "completed"
            ? CircleCheck
            : CircleHelp
      }
      title={a.title}
      subtitle={
        <>
          {queueGoAhead ? "Next up · Review and start" : waitingTarget ? handBackReason(waitingTarget) : a.kind === "reconciliation_required"
            ? "Outcome unknown"
            : results[a.attention_id] ?? (a.kind === "approval"
              ? "Approval needed"
              : a.kind.replaceAll("_", " "))}{" "}
          · <Age at={a.created_at} suffix=" ago" />
          {where && <WhereLine projectId={waitingTask?.project_id} />}
          {a.approval_id && approvalDetails[a.approval_id]?.kind === "question" && <span className="work-row-line">{approvalDetails[a.approval_id]?.detail}</span>}
          {/* Answers to a preview's questions, left here to send later. */}
          {a.task_id && unsentAnswers(a.task_id) && <span className="work-row-line">Your answers about its preview aren’t sent yet</span>}
        </>
      }
      chevron
      onClick={() => {
        if (queueGoAhead) ui.push({ screen: "attention", attentionId: a.attention_id });
        else if (waitingTarget && waitingTask) setHandBackId(waitingTask.task_id);
        else if (a.approval_id) setChosen(a.approval_id);
        else if (a.task_id) {
          const t = session.state.get().tasks[a.task_id];
          if (t && (a.kind === "completed" || a.kind === "failure")) ui.push({ screen: "receipt", taskId: t.task_id, ...(a.run_id ? { runId: a.run_id } : {}) });
          else if (t) open(t);
        }
      }}
    />
    {/* What the task may do, on its card but under its button, so at large
        text the first request still fits on the first screen. */}
    {mayLines[a.attention_id] && <p className="work-attention-may">{mayLines[a.attention_id]}</p>}
    {a.approval_id && <ApprovalQuickActions approvalId={a.approval_id} />}
    </SeenRow>;
  };
  return (
    <>
      {lead && groups[0] && <div className="group work-lead">{shown(groups[0])}</div>}
      <div className="group">{(lead ? groups.slice(1) : groups).map(shown)}</div>
      {!attention.length && !quiet && (
        <p className="work-muted">{review ? "Nothing ready to review in this project." : "Nothing waiting for you in this project."}</p>
      )}
      {handBackTask && <HandBack task={handBackTask} reviewOnly onClose={() => setHandBackId(null)} />}
      <Sheet
        className="work-sheet"
        open={chosen !== null}
        onClose={() => setChosen(null)}
        title="Review request"
        urgentExcept={task ? { taskId: task.task_id } : undefined}
        footer={
          task && isActiveTask(task) ? <StopTask task={task} /> : undefined
        }
      >
        {approval ? (
          <WorkApproval
            key={`${approval.approval_id}/${approval.requested_at}`}
            approval={approval}
            task={task}
            onOpenTask={() => {
              if (!task) return;
              setChosen(null);
              ui.openTask(task.project_id, task.task_id);
            }}
          />
        ) : (
          <Notice tone="unknown">
            {approvalsGranted ? "This request isn’t available in the computer’s current state." : APPROVALS_NOT_GRANTED}
          </Notice>
        )}
      </Sheet>
    </>
  );
}
/**
 * A list's row of `ids`, which count as seen, their notifications done with,
 * once the person has looked at it for a moment while using the app
 * (`useHandledWhenSeen`). Folded away in a closed group, it isn't seen.
 */
export function SeenRow({ ids, className, as: Tag = "div", children }: { ids: readonly string[]; className?: string; as?: "div" | "li"; children: ReactNode }) {
  const { platform, profile } = useApp();
  const ref = useHandledWhenSeen(platform, profile.profileId, ids, useOnScreen());
  return <Tag ref={ref} className={className}>{children}</Tag>;
}

/** The computer sends a device without the approvals grant no approval records. */
export const APPROVALS_NOT_GRANTED = "This device was not granted approvals, so it can’t show or answer this request. Answer it on the computer or from a device that can.";

export function errorText(error: unknown, name: string) {
  return describeError(error, name);
}

/**
 * What a card needs to answer one approval, wherever it appears: the reason
 * or answer kept on the device until the computer accepts it, and decisions
 * held while offline or while an earlier decision is unconfirmed.
 */
// The host request time is an identity, not a clock to compare with this device.
const approvalInstances = new WeakMap<HostSession, Map<string, string>>();

export function useApprovalDecision(approval: Approval) {
  const { session, profile, platform } = useApp();
  const online = useLink().state === "online";
  // Caught up, so the request shown is the computer's current one.
  const caughtUp = useHost((s) => s.caughtUp);
  const current = online && caughtUp;
  const [text, setText, saved] = useLocalText(
    // Not the revision: a failed hand-off reopens the same request at a new one.
    `${profile.profileId}.approval.${approval.approval_id}.${approval.requested_at}`,
  );
  let instances = approvalInstances.get(session);
  if (!instances) { instances = new Map(); approvalInstances.set(session, instances); }
  const instance = `${approval.approval_id}/${approval.requested_at}`;
  const match = (c: Command, pending: PendingCommand) =>
    (c.command === "resolve_approval" ? c.approval_id === approval.approval_id
      : c.command === "agent_login" && c.action.action === "answer_code" && c.action.approval_id === approval.approval_id)
    && instances.get(pending.id) === instance;
  const uncertain = useUnconfirmedCommand(match);
  const send = async (body: Command, label: string, expectedRevision?: number) => {
    try {
      const commandId = uuidv7(Date.now());
      instances.set(commandId, instance);
      const receipt = await session.command(body, { commandId, label, expectedRevision });
      await commandResult(session, receipt);
    } catch (error) {
      const unconfirmed = Object.values(session.commands.get()).some(
        (c) =>
          match(c.body, c) &&
          (c.phase === "unknown" || c.error?.code === "outcome_unknown"),
      );
      if (unconfirmed)
        throw {
          error: {
            code: "outcome_unknown",
            message:
              "The computer hasn’t confirmed your decision. Reconnect to reconcile it.",
          },
        };
      throw error;
    }
  };
  const onDecide = async (decision: ApprovalDecision) => {
    // Letting an agent sign in or post on GitHub as the owner asks for this
    // phone's unlock.
    if ((approval.kind === "login_use" || approval.kind === "github_post") && decision.decision === "allow") await confirmHere(platform, approval.title);
    await send({ command: "resolve_approval", approval_id: approval.approval_id, decision }, DECISION_LABEL[decision.decision], approval.revision);
    setText("");
    announce(DECIDED[decision.decision]);
  };
  const onCode = async (code: string) => {
    await send({ command: "agent_login", action: { action: "answer_code", approval_id: approval.approval_id, code } }, "Code");
    announce("Code sent.");
  };
  // The ways forward the computer says it takes, in place of Decline and
  // Allow where they differ (Switch to Work, Turn into a task, Not now).
  const actions = useApprovalWayButtons(approval);
  return {
    /** Spread onto ApprovalCard. */
    card: {
      ...(actions ? { actions } : {}),
      draft: { text, onChange: setText },
      disabled: !current || uncertain,
      disabledReason: uncertain
        ? "The computer hasn’t confirmed your decision. Reconnect to reconcile it."
        : online ? "Checking this request is still current…" : "Connect to the computer to answer this request.",
      onDecide,
      onCode,
    },
    saved,
    match,
  };
}
/** Said once a decision is sent: what it means for the work. */
const DECIDED: Record<ApprovalDecision["decision"], string> = {
  allow: "Allowed.",
  deny: "Declined. The agent carries on without it.",
  answer: "Answer sent.",
};
const DECISION_LABEL: Record<ApprovalDecision["decision"], string> = {
  allow: "Allow",
  deny: "Decline",
  answer: "Answer",
};

function WorkApproval({
  approval,
  task,
  onOpenTask,
}: {
  approval: Approval;
  task: Task | undefined;
  onOpenTask: () => void;
}) {
  const { profile } = useApp();
  const grant = useHost((s) => s.grants?.approvals === true);
  const { card, saved, match } = useApprovalDecision(approval);
  return (
    <div className="work-stack">
      <ApprovalCard
        approval={approval}
        agent={task ? harnessLabel(task.harness) : "Agent"}
        myDeviceId={profile.deviceId}
        canDecide={grant}
        {...card}
        onOpenTask={task ? onOpenTask : undefined}
      />
      {!saved && <UnsavedAnswer />}
      <CommandFeedback match={match} hideAccepted={approval.state === "pending"} runEnded={approval.state === "cancelled"} />
    </div>
  );
}

export function UnsavedAnswer() {
  return (
    <Notice tone="stop">
      Your answer hasn’t been saved on the device. Keep this screen open.
    </Notice>
  );
}
