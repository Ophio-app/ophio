// SPDX-License-Identifier: Apache-2.0
import { ChevronRight, TriangleAlert } from "lucide-react";
import { useEffect, useState } from "react";
import { describeError, describeProtocolError, shallowEqual, useApp, useHost, useLink } from "../../app/context";
import { REVIEW_FLOW } from "../../app/ui";
import { Button } from "../../design/Button";
import { TextArea } from "../../design/Field";
import { Empty, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { commandResult } from "../../host/commandResult";
import { OutcomeUnknownError } from "../../host/session";
import { Screen, useRoute } from "../../shell/Screen";
import type { QueueItem, QueueView, Task } from "../../protocol/types";
import type { RouteProps } from "../types";
import { useLocalText } from "../work/local";
import { useOpenTask } from "../work/shared";
import { pendingQueueItems, useQueue } from "../work/useQueue";
import { agentLabel, modeSupport, writerHold, writerHoldText } from "./intent";
import { LaunchLine } from "./LaunchLine";
import { modeLabel } from "./ModePicker";
import { confirmed } from "./send";
import { startSummary } from "./startSummary";
import { bundleSummary } from "./timeline";
import { taskBadge } from "./TaskBar";

type Props = RouteProps<"review">;

export function QueueReview({ route, itemId }: Props & { itemId: string }) {
  const { queue, error, reload } = useQueue(route.projectId);
  const task = useHost((s) => route.taskId ? s.tasks[route.taskId] : undefined);
  const item = queue?.items.find((i) => i.item_id === itemId);
  return queue && item && task ? <QueuedRequest key={itemId} route={route} task={task} item={item} queue={queue} reload={reload} />
    : <Screen title="Review queued task">{queue ? <LeftNextUp route={route} task={task} queue={queue} />
      : <Empty title={error ?? "Request not loaded"} action={<Button onClick={reload}>Reload request</Button>}>Connect to the computer to read this saved request.</Empty>}</Screen>;
}

/** The request is no longer waiting, so the way on is its task or back. */
function LeftNextUp({ route, task, queue }: Props & { task: Task | undefined; queue: QueueView }) {
  const { ui, profile } = useApp();
  const { tab } = useRoute();
  const openTask = useOpenTask();
  const run = useHost((s) => task?.current_run ? s.runs[task.current_run] : undefined);
  return <Empty title="This task has left Next up" action={<>
    {task && <Button variant="primary" onClick={() => openTask(task)}>Open task</Button>}
    <Button onClick={() => ui.finish(REVIEW_FLOW, tab, route)}>Close review</Button>
  </>}>{task ? `${task.title} · ${taskBadge(task, run, queue).label}` : `It is no longer on ${profile.name}.`}</Empty>;
}

function QueuedRequest({ route, task, item, queue, reload }: Props & { task: Task; item: QueueItem; queue: QueueView; reload(): void }) {
  const app = useApp();
  const { session, profile } = app;
  const online = useLink().state === "online";
  const permitted = useHost((s) => s.grants?.tasks === true && s.caughtUp);
  const harnesses = useHost((s) => s.harnesses);
  const project = useHost((s) => s.projects[task.project_id]);
  const run = useHost((s) => task.current_run ? s.runs[task.current_run] : undefined);
  const held = useHost((s) => writerHold(queue.writer, s), shallowEqual);
  const openTask = useOpenTask();
  const signature = requestSignature(task, item);
  const [reviewed, setReviewed] = useState({ task: task.revision, queue: queue.revision, signature });
  const [text, setText, saved] = useLocalText(`${profile.profileId}.queue-prompt.${item.item_id}`, item.request?.prompt ?? "");
  const [pending, setPending] = useState<"start" | "save" | "later" | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  // The id of a start whose answer was lost; the session learns its fate.
  const [unknown, setUnknown] = useState<string | null>(null);
  const edited = text !== item.request?.prompt;
  // Once this start is sent, the queue moves on under it: the checks that
  // cleared it no longer describe this request.
  const starting = pending === "start";
  const changed = !starting && pending !== "later" && (signature !== reviewed.signature || (edited && (reviewed.task !== task.revision || reviewed.queue !== queue.revision)));
  const harness = harnesses.find((h) => h.kind === task.harness);
  const support = modeSupport(harness, task.mode ?? "work");
  const heldText = (task.mode ?? "work") === "work" && held ? writerHoldText(held, project?.name ?? "this project") : null;
  const blocked = starting ? null
    : !online ? "Connect to the computer to start this task."
    : !permitted ? "This device cannot start tasks right now."
    : unknown ? `Waiting for ${profile.name} to say whether it started.`
    : changed ? "The queue or task changed. Review the current request first."
    : !item.request ? "This computer did not provide the saved request."
    : pendingQueueItems(queue)[0]?.item_id !== item.item_id ? EARLIER
    : item.state.state === "blocked" ? item.state.reason
    : heldText ? heldText
    : harness?.capabilities.start_run.state !== "available" || support.availability.state !== "available" ? "This agent cannot start the saved mode right now."
    : edited ? "Save your prompt changes, then review before starting." : null;
  // Held only by work ahead of it, the go-ahead can be given now: it starts
  // by itself once that work ends, and the person needn't wait to answer. A
  // paused Next up starts nothing by itself, so there it waits as before.
  const later = !queue.paused && !!blocked && (blocked === heldText || blocked === EARLIER) && !edited;
  const startLater = async () => {
    setPending("later"); setFailure(null);
    try {
      await commandResult(session, await session.command({ command: "queue_set_authorization", item_id: item.item_id, authorization: "auto_start" },
        { label: "Start when it’s free", expectedRevision: queue.revision }));
      app.ui.closeReviews((r) => r.queueItemId === item.item_id);
      confirmed(app, task.project_id, task.task_id, `“${task.title}” starts by itself when it’s free`);
    } catch (e) {
      setFailure(describeError(e, profile.name));
      reload();
    } finally { setPending(null); }
  };
  // Started, here or anywhere: this Review closes in the tab it was opened
  // from and the person stays where they are.
  const started = () => {
    app.ui.closeReviews((r) => r.queueItemId === item.item_id);
    confirmed(app, task.project_id, task.task_id, `Started “${task.title}”`);
  };
  useEffect(() => {
    if (!unknown) return;
    const test = () => {
      const c = session.commands.get()[unknown];
      if (!c || c.phase === "sending" || (c.phase === "unknown" && (!online || session.sameCommandHistory(c.historyId)))) return;
      setUnknown(null);
      setFailure(null);
      // Only a start starts the task; a saved prompt leaves it waiting here.
      const edit = c.body.command !== "queue_start";
      if (c.phase === "accepted" || c.phase === "duplicate") return edit ? reload() : started();
      setFailure(c.phase === "rejected" ? (c.error ? describeProtocolError(c.error) : "Refused.")
        : c.phase === "not_delivered" ? `${profile.name} never received it, so ${edit ? "your prompt changes weren’t saved" : "nothing started"}.`
        : edit ? `${profile.name} restarted, so it can’t say whether your prompt changes were saved. Check the saved prompt before saving again.`
        : `${profile.name} restarted, so it can’t say whether this started. Check the task before starting it again.`);
      reload();
    };
    test();
    return session.commands.subscribe(test);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [unknown, online]);
  const submit = async (edit: boolean) => {
    setPending(edit ? "save" : "start"); setFailure(null);
    try {
      await session.command(edit ? { command: "queue_edit_prompt", item_id: item.item_id, prompt: text }
        : { command: "queue_start", item_id: item.item_id, task_revision: task.revision }, { label: edit ? "Save queued prompt" : "Start queued task", expectedRevision: queue.revision });
      if (edit) reload(); else started();
    } catch (e) {
      setFailure(describeError(e, profile.name));
      if (e instanceof OutcomeUnknownError) setUnknown(e.commandId);
      reload();
    } finally { setPending(null); }
  };
  // Where, who, what access and when, beside the button that starts it.
  const summary = startSummary({
    project: project?.name ?? "this project", alone: false, agent: harnesses.find((h) => h.kind === task.harness)?.display_name ?? task.harness,
    harness: task.harness, policy: task.launch_policy, mode: task.mode ?? "work",
    placement: later ? "queue" : "start_now", authorization: item.authorization, paused: queue.paused,
  });
  if (!pending && !unknown && (item.state.state === "started" || task.state !== "accepted"))
    return <Screen title="Review queued task" subtitle={task.title}><LeftNextUp route={route} task={task} queue={queue} /></Screen>;
  return <Screen className="review-screen" title="Review queued task" subtitle={task.title} footer={<div className="review-foot">
    {blocked && (blocked === heldText && held?.task ? <div className="review__held">
      <p className="review-foot__why">{blocked}</p>
      <Button size="sm" variant="secondary" trailingIcon={ChevronRight} onClick={() => openTask(held.task!)}>{held.unresolved ? "Check it" : "Open it"}</Button>
    </div> : <p className="review-foot__why">{blocked}</p>)}
    {queue.paused && !blocked && <p className="review-foot__why">Only this task will start. The rest of Next up stays paused.</p>}
    <p className={summary.risk ? "review-foot__summary launch-line__risk" : "review-foot__summary"}>
      {summary.risk && <TriangleAlert aria-hidden strokeWidth={2.2} />}<span>{summary.text}</span>
    </p>
    {later ? <Button variant="primary" block size="lg" disabled={!!pending || item.authorization === "auto_start"} pending={pending === "later"} onClick={() => void startLater()}>{item.authorization === "auto_start" ? "Starts by itself when it’s free" : "Start when it’s free"}</Button>
      : <Button variant="primary" block size="lg" disabled={!!blocked || !!pending} pending={starting} onClick={() => void submit(false)}>{starting ? "Starting…" : queue.paused ? "Start only this task" : "Start queued task"}</Button>}
  </div>}><div className="review">
    {queue.paused && <Notice title="Next up is paused">{queue.pause_reason ?? "Automatic starts are paused."}</Notice>}
    {failure && <Notice tone="warn">{failure}</Notice>}
    {changed && <Notice tone="warn" title="The task or queue changed" action={<Button onClick={() => { setReviewed({ task: task.revision, queue: queue.revision, signature }); setFailure(null); }}>Review current request</Button>}>
      <p>{task.title} · {taskBadge(task, run, queue).label}</p><p>Saved prompt: {item.request?.prompt ?? "Not reported"}</p>{edited && <p>Your edits are kept below. Review them before saving or starting.</p>}
    </Notice>}
    <Section title="Saved request"><TextArea label="Queued prompt" value={text} onChange={setText} rows={4} maxRows={12} />
      {!saved && <Notice tone="warn">Your edits could not be saved on this device. Keep this screen open.</Notice>}
      {edited && <Button disabled={!online || !permitted || changed || !text.trim() || !!unknown || starting} pending={pending === "save"} onClick={() => void submit(true)}>Save prompt changes</Button>}
      {item.request?.bundle && <p>{bundleSummary(item.request.bundle)} · saved with this request</p>}
    </Section>
    <Section title="Where and who"><p className="review__strong">{project?.name} · {profile.name}</p><p className="review__mono">{project?.host_path}</p><p>{agentLabel(harnesses, { harness: task.harness, model: task.model, effort: task.effort })}</p><p>Mode: {modeLabel(task.mode)}</p>
      <LaunchLine harness={task.harness} policy={task.launch_policy} mode={task.mode ?? "work"} /></Section>
    {task.mission && <Section title="Requested scope"><p>{task.mission.goal}</p>
      {task.mission.bounds.map((bound, i) => <p key={i}>{bound.text} · {bound.enforcement === "enforced" ? "Enforced by the computer" : "Requested instruction"}</p>)}
      {task.mission.completion_checks.length > 0 && <><h3>Requested checks</h3><ul>{task.mission.completion_checks.map((check, i) => <li key={i}>{check}</li>)}</ul></>}
      {task.mission.stop_point && <p>Stop when: {task.mission.stop_point} · requested instruction</p>}
    </Section>}
    <p className="review__sub">Starting uses this saved request, agent and mode. Editing the prompt withdraws earlier permission to start by itself.</p>
  </div></Screen>;
}

const EARLIER = "An earlier task is still waiting.";

/** Revisions also change when a different task stops; only this request needs review. */
function requestSignature(task: Task, item: QueueItem): string {
  return JSON.stringify([item.request, task.harness, task.model, task.effort, task.mode, task.mission, item.depends_on, item.authorization, item.authorization_expires_at]);
}
