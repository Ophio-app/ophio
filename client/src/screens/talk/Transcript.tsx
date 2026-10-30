// SPDX-License-Identifier: Apache-2.0
import { carriedPlan, sentPlan, useCarryPlan, type CarryPlan } from "./carryPlan";
import { continuationSelection, planReviewSelection } from "./continuationSelection";
import { ToolInput, toolInput } from "./toolInput";
import { unstartedQueueItem } from "../work/queueStatus";
import { taskActivity, taskStatus } from "../work/taskActivity";
import { awaitingInputBack } from "../../state/handBack";
import { ReproductionReceipt } from "../annotate/ReproductionReview";
//
// One task's conversation. The person's words and the agent's replies lead;
// tool steps fold into quiet groups that open on demand; approvals are
// answered where they appear; each message says how far it really got.

import { ChevronDown, CircleCheck, CircleHelp, CircleSlash, CircleX, Paperclip, ReceiptText, ScrollText, ShieldCheck, ShieldQuestion, ShieldX, Wrench } from "lucide-react";
import { memo, useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { describeError, shallowEqual, useApp, useHost, useLink } from "../../app/context";
import { projectFileLink } from "./fileLink";
import { Age } from "../../design/Age";
import { ApprovalCard } from "../../design/ApprovalCard";
import { Button } from "../../design/Button";
import { Markdown } from "../../design/Markdown";
import { Notice } from "../../design/Notice";
import { StateLine, type DeliveryPhase } from "../../design/StateLine";
import { clockTime, useNow } from "../../design/time";
import type { Approval, DeliveryState, QueueItem, Task, TaskMode } from "../../protocol/types";
import { UnsavedAnswer, useApprovalDecision } from "../work/shared";
import { withoutOutcome } from "../../state/outcomeReport";
import { isTaskRunning, liveTextForTask, pendingApprovals, taskRuns } from "../../state/projection";
import { HeldMessage } from "./HeldMessage";
import { ShownCard } from "./Shown";
import { useStore } from "../../state/store";
import { handBackHistory, withHandBacks } from "./handBackHistory";
import { NotAllowed, useMay } from "../../shell/NotAllowed";
import { modelName } from "./catalog";
import { CONVERSATION_SUMMARY_PROMPT } from "./conversation";
import { SUMMARY_PROMPT } from "./summarize";
import { bundleSummary, keepUnchanged, NOT_RUN, rows, runEnd, stepSummary, stepsSummary, type ActivityItem, type Row, type Step } from "./timeline";

// Only plan steps need the earlier text; every other row keeps a stable prop so RowView stays memoized.
const NO_PLANS: string[] = [];

const DELIVERY: Record<DeliveryState, DeliveryPhase> = {
  host_accepted: "agent_unconfirmed",
  queued_for_boundary: "queued_for_boundary",
  native_delivered: "native_delivered",
  failed: "not_delivered",
  unknown: "unknown",
  cancelled: "not_delivered",
};

export function Transcript({ task, agent }: { task: Task; agent: string }) {
  const { session, profile } = useApp();
  const plan = useCarryPlan(task);
  const taskId = task.task_id;
  const handBacks = useStore(handBackHistory(session), records => records);
  const online = useLink().state === "online";
  const active = useHost((s) => isTaskRunning(s, task));
  const currentRun = useHost(s => {
    const run = s.runs[task.current_run ?? ""];
    return run?.task_id === taskId ? run : undefined;
  });
  const control = useHost(s => s.control[task.target_id ?? ""]);
  const status = taskStatus(task, currentRun, control);
  const waitingStatus = awaitingInputBack(task, currentRun, control) ? status : null;
  const targetKnown = useHost((s) => s.targets.some((t) => t.target_id === task.target_id));
  const blocking = useHost(s => pendingApprovals(s, taskId).filter(a => a.run_id === task.current_run).map(a => a.kind), shallowEqual);
  const approvalBlocked = blocking.length > 0;
  const waiting = approvalBlocked || !!waitingStatus || task.state === "awaiting_decision" || currentRun?.state === "awaiting_input" || currentRun?.state === "awaiting_approval";
  const runs = useHost(s => taskRuns(s, taskId), shallowEqual);
  const harness = useHost(s => s.harnesses.find(h => h.kind === task.harness));
  const queue = useHost(s => s.queues[task.project_id]);
  const queued = unstartedQueueItem(task, runs, queue);
  // Before any run the first message has reached only the computer; the
  // queue item, when there is one, says where it waits.
  const unstarted = task.state === "accepted" && !task.current_run && runs.length === 0;
  const timeline = useHost((s) => s.timelines[taskId]);
  // Rows keep their identity until the timeline or runs change, so the clock and streamed text leave RowView memoized,
  // and a new item leaves every row it does not change as it was, so only that row renders.
  const loadedItems = timeline?.loaded ? timeline.items : null;
  const shownRows = useRef<Row[]>([]);
  const timelineRows = useMemo(() => shownRows.current = keepUnchanged(shownRows.current, loadedItems ? rows(loadedItems, runs) : []), [loadedItems, runs]);
  const [error, setError] = useState<string | null>(null);
  const [older, setOlder] = useState(false);
  const allowed = useMay("tasks");

  const request = useRef(0);
  const load = useCallback((earlier: boolean) => {
    const id = ++request.current;
    setOlder(earlier);
    return session.loadTimeline(taskId, earlier).then(() => {
      if (id === request.current) setError(null);
    }).catch((e: unknown) => {
      if (id === request.current) setError(describeError(e, profile.name));
    }).finally(() => {
      if (id === request.current) setOlder(false);
    });
  }, [session, taskId, profile.name]);

  useEffect(() => session.watchTimeline(taskId), [session, taskId]);

  useEffect(() => {
    setError(null);
    if (allowed && online) void load(false);
    return () => { request.current++; };
  }, [load, allowed, online]);

  if (!timeline?.loaded) {
    return !allowed ? (
      <NotAllowed what="read or steer tasks" permission="tasks" />
    ) : error ? (
      <Notice tone="warn" title="Couldn’t load this conversation" action={<Button size="sm" onClick={() => load(false)}>Try again</Button>}>
        {error}
      </Notice>
    ) : (
      <div className="talk-loading" aria-busy="true" aria-label="Loading the conversation">
        <span />
        <span />
        <span />
      </div>
    );
  }

  const all = withHandBacks(timelineRows, handBacks.filter(r => r.taskId === taskId && session.sameCommandHistory(r.history)));
  // In a conversation each message is a run; the person's words mark it.
  const list = task.conversation ? all.filter(r => r.kind !== "run_started" || r.number === 1) : all;
  // The model can change between a conversation's messages, so a reply from
  // another model than the one before it says which.
  const switched = new Map<string, string>();
  if (task.conversation) {
    let before: string | null = null;
    for (const r of list) {
      if (r.kind !== "run_ended") continue;
      const run = runs.find(x => x.run_id === r.runId);
      const model = run ? run.actual_model ?? run.requested_model : null;
      if (before !== null && model && model !== before) switched.set(r.runId, modelName(model, harness));
      if (model) before = model;
    }
  }
  const firstUser = list.findIndex(r => r.kind === "user");
  const lastUser = list.findLastIndex((r) => r.kind === "user");
  const inlinePlan = plan?.latest ? plan : null;
  const planMessage = inlinePlan ? list.findLastIndex(r => r.kind === "agent" && !!r.text.trim() && inlinePlan.text.includes(r.text.trim()) && timeline.items.some(i => `s${i.seq}` === r.key && i.run_id === inlinePlan.runId)) : -1;
  const planStep = inlinePlan ? list.findLastIndex(r => r.kind === "steps" && r.steps.some(st => st.run_id === inlinePlan.runId && st.item.tool === "ExitPlanMode")) : -1;
  const planPosition = planMessage >= 0 ? planMessage : planStep >= 0 ? planStep : list.length - 1;
  const carries = (row: Row) => row.kind !== "user" ? undefined : sentPlan(row.text, row.carried, runs, timeline);


  return (
    <div className="talk-log">
      {timeline.hasMore && (
        <div className="talk-log__earlier">
          <Button variant="quiet" size="sm" pending={older} onClick={() => load(true)}>
            {older ? "Loading earlier messages" : "Show earlier messages"}
          </Button>
        </div>
      )}
      {error && (
        <Notice tone="warn" title="Couldn’t load earlier messages">
          {error}
        </Notice>
      )}
      <ol className="talk-log__list" aria-label="Conversation">
        {list.map((r, i) => (
          <li key={r.key} className={`talk-log__item talk-log__item--${r.kind}`}>
            {inlinePlan && i === planPosition && planMessage < 0 && <FinishedPlan task={task} plan={inlinePlan} showText />}
            <RowView row={r} carries={carries(r)} shownPlans={r.kind === "steps" && r.steps.some(st => st.item.tool === "ExitPlanMode") ? list.slice(0, i).flatMap(row => row.kind === "agent" ? [row.text] : []).concat(inlinePlan && i >= planPosition && planMessage < 0 ? [inlinePlan.text] : []) : NO_PLANS} showDelivery={r.kind === "user" && (i === lastUser || r.delivery === "failed" || r.delivery === "unknown" || r.delivery === "queued_for_boundary" || r.delivery === "cancelled")} agent={agent} taskId={taskId} conversation={!!task.conversation} replyModel={r.kind === "run_ended" ? switched.get(r.runId) : undefined} currentMode={r.kind === "run_ended" && r.runId === task.current_run ? task.mode ?? "work" : undefined} unstarted={i === firstUser && unstarted} queued={i === firstUser ? queued : undefined} />
            {inlinePlan && i === planPosition && planMessage >= 0 && <FinishedPlan task={task} plan={inlinePlan} showText={false} />}
          </li>
        ))}
        <LiveReplies taskId={taskId} />
      </ol>
      {inlinePlan && list.length === 0 && <FinishedPlan task={task} plan={inlinePlan} showText />}
      {/* A paused agent's bar, just below, says so with its Resume. */}
      {active && !(waitingStatus && targetKnown) && <Working task={task} online={online} waiting={waiting ? (approvalBlocked ? blocking.every(k => k === "question") ? "Waiting for your answer" : "Waiting for your approval" : waitingStatus ?? "Waiting for you") : null} status={status} />}
    </div>
  );
}

// Streamed text, live activity and the clock change many times a second; each
// is read by the small part that shows it, so the conversation above stays put.

function LiveReplies({ taskId }: { taskId: string }) {
  const live = useHost((s) => liveTextForTask(s, taskId), shallowEqual);
  return live.map((l) => (
    <li key={l.key} className="talk-log__item talk-log__item--agent">
      <div className="talk-agent talk-agent--live" aria-live="off">
        <Markdown text={withoutOutcome(l.text)} />
      </div>
    </li>
  ));
}

function Working({ task, online, waiting, status }: { task: Task; online: boolean; waiting: string | null; status: string | null }) {
  const activity = useHost((s) => taskActivity(s, task));
  return (
    <p className="talk-working" role="status">
      {online && waiting === null && <span className="stateline__spinner" aria-hidden />}
      <span>{waiting ?? activity?.text ?? (task.state === "dispatching" ? "Starting" : status ?? "Working")}
        {!online && <> · last reported <Age at={activity?.at ?? task.updated_at} suffix=" ago" /></>}
      </span>
    </p>
  );
}

/** A first message before any run: where it waits, and whether it may start without asking while that permission lasts. */
function Unstarted({ queued, computer }: { queued?: QueueItem; computer: string }) {
  const now = useNow();
  const permission = queued ? queued.authorization === "auto_start" && (!queued.authorization_expires_at || queued.authorization_expires_at > now) ? "Can start without asking again" : "Will ask before starting" : undefined;
  return <StateLine phase={permission ? "waiting_in_next_up" : "host_accepted"} detail={permission} computer={computer} className="talk-user__state" />;
}

/** A request Øphio words for the person: the bubble says what they asked, and opens to the words sent. */
const ASKED: Record<string, string> = { [CONVERSATION_SUMMARY_PROMPT]: "Sum up so far", [SUMMARY_PROMPT]: "Summarize what happened" };

const RowView = memo(function RowView({ row, carries, showDelivery, agent, taskId, conversation, replyModel, currentMode, unstarted, queued, shownPlans }: { row: Row; carries?: string; showDelivery: boolean; agent: string; taskId: string; conversation: boolean; replyModel?: string; currentMode?: TaskMode; unstarted: boolean; queued?: QueueItem; shownPlans: string[] }) {
  const { profile, platform, ui } = useApp();
  // A stable handler keeps Markdown memoized, so a reply is parsed again only when its text changes.
  // A file in the project's folder opens in Ophio; anything else outside it.
  const root = useHost((s) => s.libraryRoots.find((r) => r.kind === "project" && r.project_id === s.tasks[taskId]?.project_id));
  const openLink = useCallback((href: string) => {
    const path = projectFileLink(href, root);
    if (path && root) ui.push({ screen: "file", rootId: root.root_id, path, taskId });
    else void platform.openExternal(href);
  }, [platform, root, ui, taskId]);
  switch (row.kind) {
    case "user": {
      // An annotation sent on its own is the whole message: it takes the bubble.
      const alone = !row.text.trim();
      const carried = carriedPlan(row.text, carries);
      const asked = ASKED[row.text];
      return (
        <div className="talk-user">
          {!alone && (asked ? (
            <details className="talk-user__text talk-user__asked">
              <summary><ScrollText aria-hidden strokeWidth={2.1} />{asked}<ChevronDown aria-hidden strokeWidth={2.1} className="talk-user__more" /></summary>
              <p>{row.text}</p>
            </details>
          ) : <p className="talk-user__text">{carried?.message ?? row.text}</p>)}
          {carried && <PlanFigure carried={carried} />}
          {row.bundle && (
            <p className={`talk-user__attach${alone ? " talk-user__attach--alone" : ""}`}>
              <Paperclip aria-hidden strokeWidth={2.1} />
              {bundleSummary(row.bundle)}
            </p>
          )}
          {row.bundle && <ReproductionReceipt bundle={row.bundle} delivery={row.delivery} />}
          {unstarted && row.delivery === "host_accepted"
            ? <Unstarted queued={queued} computer={profile.name} />
            : row.delivery === "queued_for_boundary" || row.delivery === "cancelled"
            ? <HeldMessage taskId={taskId} messageSeq={row.messageSeq} delivery={row.delivery} withdrawn={row.withdrawn} revoked={row.revoked}
                request={{ item: { kind: "user_message", text: row.text, bundle: row.bundle, delivery: row.delivery } }} />
            : showDelivery && <StateLine phase={DELIVERY[row.delivery]} computer={profile.name} className="talk-user__state" />}
        </div>
      );
    }
    case "agent":
      return (
        <div className="talk-agent">
          <Markdown text={row.text} onLink={openLink} />
        </div>
      );
    case "steps":
      return <Steps steps={row.steps} ended={row.ended} shownPlans={shownPlans} />;
    case "approval":
      return <InlineApproval approvalId={row.approvalId} runId={row.runId} agent={agent} />;
    case "run_started":
      return (
        <p className="talk-divider">
          <span>{`${row.number > 1 ? `Continued · run ${row.number}` : "Started"} · ${clockTime(row.at)}`}</span>
        </p>
      );
    case "helpers_checked":
      return <p className="talk-divider"><span>{`Helpers checked ${row.decision.by.kind === "device" && row.decision.by.device_id === profile.deviceId ? "by you" : "by a person"} · ${clockTime(row.at)} · not verified by the computer`}</span></p>;
    case "decision":
      return <p className="talk-divider"><span>{`Marked ${row.decision.resolution === "mark_failed" ? "failed" : "completed"} ${row.decision.by.kind === "device" && row.decision.by.device_id === profile.deviceId ? "by you" : "by a person"} · ${clockTime(row.at)} · not verified by the computer`}</span></p>;
    case "run_ended":
      return (
        <p className={`talk-divider talk-divider--${row.state}`}>
          {/* One text node: a phone screen reader stops on each node beside the link. */}
          <span>
            {`${runEnd(row.state, row.mode ?? currentMode, conversation)}${replyModel ? ` · ${replyModel}` : ""} · ${clockTime(row.at)}${row.withdrawn > 0 ? ` · ${row.withdrawn === 1 ? "1 held message" : `${row.withdrawn} held messages`} withdrawn` : ""}`}
          </span>
          <button type="button" className="linklike talk-divider__link" onClick={() => ui.push({ screen: "receipt", taskId, runId: row.runId })}>
            <ReceiptText aria-hidden strokeWidth={2.1} />
            What happened
          </button>
        </p>
      );
    case "receipt": {
      // Only a fully confirmed receipt earns the check; the summary says the rest.
      const Icon = { confirmed: ShieldCheck, open: ShieldQuestion, failed: ShieldX }[row.outcome];
      return (
        <p className={`talk-note talk-note--receipt talk-note--${row.outcome}`}>
          <Icon aria-hidden strokeWidth={2.1} />
          <span>{`${row.summary} · ${clockTime(row.at)}`}</span>
        </p>
      );
    }
    case "note":
      return <p className="talk-note">{row.text}</p>;
    case "shown":
      return <ShownCard shown={row.shown} taskId={taskId} at={row.at} />;
  }
});

/** An agent's plan sent with a message, quoted under its source so it never reads as the person's words. */
export function PlanFigure({ carried }: { carried: { number: number; plan: string } }) {
  return (
    <figure className="talk-user__plan">
      <figcaption>Plan from run {carried.number}</figcaption>
      <blockquote className="plan-quote"><Markdown text={carried.plan} /></blockquote>
    </figure>
  );
}

function StepIcon({ status, notRun, ended }: { status: ActivityItem["item"]["status"]; notRun: ActivityItem["item"]["not_run"]; ended: boolean }) {
  const online = useLink().state === "online";
  if (notRun) return <CircleSlash className="talk-steps__icon" aria-label={NOT_RUN[notRun].label} strokeWidth={2.1} />;
  switch (status) {
    case "running":
      if (ended) return <CircleHelp className="talk-steps__icon" aria-label="Ended without a result" strokeWidth={2.1} />;
      return online ? <span className="stateline__spinner talk-steps__spin" aria-label="Running" /> : <CircleHelp className="talk-steps__icon" aria-label="Last reported running" />;
    case "succeeded":
      return <CircleCheck className="talk-steps__icon talk-steps__icon--ok" aria-label="Done" strokeWidth={2.1} />;
    case "failed":
      return <CircleX className="talk-steps__icon talk-steps__icon--failed" aria-label="Failed" strokeWidth={2.1} />;
    default:
      return <CircleHelp className="talk-steps__icon" aria-label="Result unknown" strokeWidth={2.1} />;
  }
}

export function Steps({ steps, ended, shownPlans = [] }: { steps: Step[]; ended: boolean; shownPlans?: string[] }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  const summary = stepsSummary(steps, ended);
  const s = { ...summary, running: summary.running && !ended };
  const online = useLink().state === "online";
  return (
    <div className={`talk-steps${open ? " is-open" : ""}`}>
      <button type="button" className="talk-steps__head" aria-expanded={open} aria-controls={id} onClick={() => setOpen((o) => !o)}>
        {s.running && online ? <span className="stateline__spinner talk-steps__spin" aria-hidden /> : <Wrench className="talk-steps__lead" aria-hidden strokeWidth={2.1} />}
        <span className="talk-steps__latest">{s.latest}{s.running && !online && <> · last reported <Age at={steps.at(-1)?.at ?? 0} suffix=" ago" /></>}</span>
        <span className="talk-steps__count">
          {s.count}
          {s.failed > 0 && <span className="talk-steps__failed"> · {s.failed} failed</span>}
          {(["denied", "stopped"] as const).map(why => s.notRun[why] > 0 && <span key={why}> · {s.notRun[why]} {NOT_RUN[why].counted}</span>)}
        </span>
        <ChevronDown className="talk-steps__chev" aria-hidden />
      </button>
      {open && (
        <ol className="talk-steps__list" id={id}>
          {steps.map((st) => (
            <li key={st.seq} className="talk-steps__row">
              <StepIcon status={st.item.status} notRun={st.item.not_run} ended={ended} />
              <div className="talk-steps__body">
                <p className="talk-steps__summary">{stepSummary(st, ended)}</p>
                <p className="talk-steps__tool">
                  {st.item.tool === "ExitPlanMode" ? "Plan" : st.item.tool} · {clockTime(st.at)}
                </p>
                {st.item.detail && (planAlreadyShown(st, shownPlans)
                  ? <p className="talk-steps__detail">Plan shown above.</p>
                  : <ToolInput tool={st.item.tool} detail={st.item.detail} />)}
                {st.output && !planAlreadyShown(st, shownPlans, st.output) && <pre className="talk-steps__detail" aria-label="Result">{st.output}</pre>}
              </div>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

function InlineApproval({ approvalId, runId, agent }: { approvalId: string; runId: string | null; agent: string }) {
  const { session } = useApp();
  const link = useLink();
  const approval = useHost((s) => s.approvals[approvalId]);
  const held = !!approval;
  const readable = useHost((s) => s.grants?.approvals === true);
  // A decision on an earlier run of a long task is read with that run.
  useEffect(() => {
    if (held || !runId || !readable || link.state !== "online" || !session.readsHistory) return;
    void session.loadRun(runId).catch(() => undefined);
  }, [held, runId, readable, link.state, session]);
  if (!approval) return <p className="talk-note">An approval request that this device can’t see.</p>;
  return <InlineDecision key={`${approval.approval_id}/${approval.requested_at}`} approval={approval} agent={agent} />;
}

function InlineDecision({ approval, agent }: { approval: Approval; agent: string }) {
  const { profile } = useApp();
  const canDecide = useHost((s) => s.grants?.approvals ?? false);
  const { card, saved } = useApprovalDecision(approval);
  return (
    <>
      <ApprovalCard approval={approval} agent={agent} myDeviceId={profile.deviceId} canDecide={canDecide} {...card} />
      {!saved && <UnsavedAnswer />}
    </>
  );
}

function FinishedPlan({ task, plan, showText }: { task: Task; plan: CarryPlan; showText: boolean }) {
  const { ui } = useApp();
  return <div className="talk-plan">
    {showText && <Markdown text={plan.text} />}
    <div className="talk-plan__actions" role="group" aria-label="Plan next steps">
      <Button variant="primary" onClick={() => planReviewSelection(ui).set({ taskId: task.task_id, send: true })}>Carry out this plan</Button>
      <Button variant="secondary" onClick={() => continuationSelection(ui).set({ taskId: task.task_id })}>Ask for changes</Button>
    </div>
  </div>;
}

function planAlreadyShown(step: Step, shownPlans: string[], detail = step.item.detail): boolean {
  if (step.item.tool !== "ExitPlanMode" || !detail) return false;
  const plan = toolInput("ExitPlanMode", detail)?.text.trim();
  return !!plan && shownPlans.some(text => text.includes(plan));
}
