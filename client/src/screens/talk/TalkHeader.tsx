// SPDX-License-Identifier: Apache-2.0
//
// Talk's own header, as a messaging app's chat header: the task's name on one
// line and how it stands under it, with nothing of the project or the app
// around it. An open task is the whole screen, as a chat is: the tabs step
// aside, and the back arrow goes to the project's conversations, or from a
// task to Work's tasks, with the tabs up, as a chat's goes back to its chats,
// counting what needs the person. A conversation's line names its model. The ⋯
// holds the task's pictures, instructions and record (D-059). This task's
// Stop ends the row; every other running task's is one row and one tap away
// (D-054).

import { ArrowLeft, ChevronRight, CircleHelp, Eye, Hourglass, KeyboardOff, LockKeyhole, MessageSquareReply, MoreHorizontal, Pause, Square } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useApp, useCommand, useHost, useLink, useUi } from "../../app/context";
import { Age } from "../../design/Age";
import { Button, IconButton } from "../../design/Button";
import { useNow } from "../../design/time";
import type { Project, Run, Task } from "../../protocol/types";
import { isTaskRunning, openAttention } from "../../state/projection";
import { useStore } from "../../state/store";
import { tabLabel } from "../../app/ui";
import { StatusButton } from "../../shell/ContextHeader";
import { useChatShown, useRailOpen } from "../../shell/focus";
import { hideKeyboard, useKeyboardOpen } from "../../shell/keyboard";
import { useMay } from "../../shell/NotAllowed";
import { StatusSheet } from "../../shell/StatusSheet";
import { StopTask } from "../control/StopTask";
import { inputState, useRail } from "../control/SafetyStrip";
import { holdsDisplay, taskDisplayId, taskDisplayLabel } from "../control/taskDisplay";
import { PrChip } from "../github/PrSheet";
import { RenameSheet } from "../work/RenameTask";
import { taskAttentionReason } from "../work/taskActivity";
import { AccessBar } from "./AccessBar";
import { MODES } from "./ModePicker";
import { conversationBadge } from "./conversation";
import { MediaSheet, useMediaPermitted } from "./RecentMedia";
import { TaskMoreSheet, taskHeadingFocus, taskRowBadge } from "./TaskBar";
import "./talkHeader.css";

type HeaderSheet = "more" | "media" | "status" | "rename";

export function TalkHeader({ project, task, run }: { project: Project; task: Task | undefined; run: Run | undefined }) {
  const { ui } = useApp();
  const [sheet, setSheet] = useState<HeaderSheet | null>(null);
  const close = () => setSheet(null);
  const kb = useKeyboardOpen();
  const online = useLink().state === "online";
  const heading = useRef<HTMLHeadingElement>(null);
  const Heading = task ? "h1" : "div";
  const conversing = useUi((s) => s.conversing);
  // A conversation is Talk's; a task, or a new one, belongs to Work.
  const conversation = task ? !!task.conversation : conversing;
  const focusRequest = useStore(taskHeadingFocus(ui), (s) => s);
  useEffect(() => {
    if (!task || focusRequest !== task.task_id) return;
    taskHeadingFocus(ui).set(null);
    heading.current?.focus();
  }, [ui, focusRequest, task]);
  const queue = useHost((s) => s.queues[project.project_id]);
  const reason = useHost((s) => task ? taskAttentionReason(s, task) : null);
  // A conversation between replies shows no state, as in Talk's list.
  const st = !task ? null : task.conversation ? conversationBadge(task, run, queue, reason) : taskRowBadge(task, run, queue, reason);
  const readOnly = task?.mode && task.mode !== "work" && !task.conversation ? MODES.find((m) => m.mode === task.mode)! : null;
  const running = useHost((s) => !!task && isTaskRunning(s, task));
  // The rail leaves this task to Talk, so Talk says which display it uses.
  const where = useHost((s) => {
    const views = Object.values(s.control);
    return task && online && isTaskRunning(s, task) && taskDisplayId(task, views) ? taskDisplayLabel(task, views, s.targets, true) : null;
  });
  const attention = useHost((s) => openAttention(s).length);
  const count = task ? attention : 0;
  // Back leads where this was opened from, when that was somewhere else.
  const from = useUi((s) => (s.returns.talk && s.stacks.talk.length === s.returns.talk.depth ? s.returns.talk : null));
  const fromTask = useHost((s) => (from?.task?.taskId ? s.tasks[from.task.taskId]?.title ?? null : null));
  const backTo = from && from.tab !== "talk" ? `Back to ${tabLabel(from.tab)}`
    : from?.task ? `Back to ${fromTask ? `“${fromTask}”` : "where you were"}`
    : conversation ? "Conversations" : "Tasks";
  const media = useMediaPermitted();

  return (
    <header className="ctx chat-head">
      {/* Without a task the tabs are up, and Work's tab carries the count; a
          conversation's back says the count is Work's, not its conversations'. */}
      <button type="button" className="chat-head__back" onClick={() => ui.leaveTask(!conversation)}
        aria-label={`${backTo}${count > 0 ? `, ${count} ${count === 1 ? "needs" : "need"} attention${conversation && from?.tab !== "work" ? " in Tasks" : ""}` : ""}`}>
        <ArrowLeft aria-hidden />
        {count > 0 && <span className="chat-head__count" aria-hidden>{count}</span>}
      </button>
      <Heading ref={heading} className="chat-head__heading" tabIndex={-1}>
        <div className="chat-head__main">
          <span className="chat-head__title">
            <span className="chat-head__title-text">{task ? task.title : conversing ? "New conversation" : "New task"}</span>
          </span>
          <span className="chat-head__sub">
            {st && (
              <span className={`chat-head__state chat-head__state--${online ? st.tone : "neutral"}`}>
                {!online && <><CircleHelp aria-hidden /><span className="chat-head__since">Last reported · </span></>}{st.label}
              </span>
            )}
            {readOnly && <span>{readOnly.label} · read-only</span>}
            {where && <span className="chat-head__where">on {where}</span>}
            {task ? !running && online && <Age className="chat-head__age" at={task.updated_at} suffix=" ago" /> : <span>in {project.name}</span>}
          </span>
        </div>
      </Heading>
      <div className="chat-head__end">
        {kb && <IconButton icon={KeyboardOff} label="Hide keyboard" onClick={hideKeyboard} />}
        <StatusButton compact quiet onOpen={() => setSheet("status")} />
        {task && <IconButton icon={MoreHorizontal} label={`More about this ${conversation ? "conversation" : "task"}`} onClick={() => setSheet("more")} />}
      </div>
      {task && <StopTask task={task} iconOnly className="chat-head__stop" />}
      {task && <TaskMoreSheet open={sheet === "more"} onClose={close} task={task} onMedia={media ? () => setSheet("media") : undefined} onRename={() => setSheet("rename")} />}
      {task && sheet === "rename" && <RenameSheet task={task} onClose={close} />}
      {task && <MediaSheet open={sheet === "media"} onClose={close} taskId={task.task_id} projectId={task.project_id} />}
      <StatusSheet open={sheet === "status"} onClose={close} />
    </header>
  );
}

/** Under the chat header: in a conversation, what its agent may reach; what
 * the task is watching or waiting for, the task that asked it, its pull
 * request, and a row summing up what else is running or holding a display. */
export function TalkPins({ task, project }: { task: Task | undefined; project?: Project | null }) {
  useChatShown(task?.task_id ?? null);
  return (
    <div className="chat-pins">
      {project && <AccessBar project={project} task={task} />}
      {task && <WatchPin task={task} />}
      {task && <AskedPin task={task} />}
      {task && <PrChip task={task} />}
      <TalkActivity task={task} />
    </div>
  );
}

/**
 * Between turns, what the agent asked Ophio to wake it for and when, or the
 * other agent's answer it waits for. The project is free meanwhile; Stop
 * watching drops the wake, and the task rests as it ended.
 */
function WatchPin({ task }: { task: Task }) {
  const execute = useCommand();
  const allowed = useMay("tasks");
  const online = useLink().state === "online";
  const running = useHost((s) => isTaskRunning(s, task));
  const now = useNow();
  const wake = task.wake;
  if (!wake || running) return null;
  if (wake.answer) return (
    <div className="chat-watch" role="group" aria-label="Waiting">
      <Hourglass className="chat-watch__icon" aria-hidden />
      <span className="chat-watch__text">Waiting for {wake.reason} · wakes it when it comes</span>
      <Button variant="quiet" size="sm" className="chat-watch__stop" disabled={!allowed || !online}
        onClick={() => void execute({ command: "stop_task", task_id: task.task_id }, { label: "Stop waiting", success: "Stopped waiting. The answer stays in its own task." })}>
        Stop waiting
      </Button>
    </div>
  );
  const when = wake.at > now ? <>wakes it <Age at={wake.at} /></> : "wakes it when the project is free";
  return (
    <div className="chat-watch" role="group" aria-label="Watching">
      <Eye className="chat-watch__icon" aria-hidden />
      <span className="chat-watch__text">Watching · {wake.reason} · {when}</span>
      <Button variant="quiet" size="sm" className="chat-watch__stop" disabled={!allowed || !online}
        onClick={() => void execute({ command: "stop_task", task_id: task.task_id }, { label: "Stop watching", success: "Stopped watching" })}>
        Stop watching
      </Button>
    </div>
  );
}

/** A task another agent asked, read-only: its answer goes back to that one,
 * which this opens. */
function AskedPin({ task }: { task: Task }) {
  const { ui } = useApp();
  const asker = useHost((s) => (task.asked_by ? s.tasks[task.asked_by] : undefined));
  if (!task.asked_by) return null;
  return (
    <div className="chat-watch" role="group" aria-label="Asked by another agent">
      <MessageSquareReply className="chat-watch__icon" aria-hidden />
      <span className="chat-watch__text">{asker ? <>Asked by “{asker.title}” · its answer goes back there</> : "Another agent asked this · its answer goes back to it"}</span>
      {asker && <Button variant="quiet" size="sm" className="chat-watch__stop" onClick={() => ui.openTask(asker.project_id, asker.task_id)}>Open</Button>}
    </div>
  );
}

/**
 * One quiet line, as a chat's pinned message: the other running tasks and any
 * display that needs a look. It opens the rail with their Stops and input
 * controls. This task's own display, driven by its agent as expected, is not
 * news; the header already says where it works. A task that uses no display
 * hears only that others are running, so their Stops stay two taps away.
 */
function TalkActivity({ task }: { task: Task | undefined }) {
  const { profile } = useApp();
  const open = useRailOpen();
  const online = useLink().state === "online";
  const { tasks, relevant, targets, controls } = useRail(task ? { taskId: task.task_id, keepDisplay: true } : undefined);
  const screenless = !!task && !taskDisplayId(task, controls);
  const notable = screenless ? [] : relevant.filter((v) => !(task && holdsDisplay(task, v) && v.input_state === "agent" && v.desktop_lock !== "locked"));
  if (!open || (tasks.length === 0 && notable.length === 0)) return null;
  const view = notable[0];
  const display = view && `${targets.find((t) => t.target_id === view.target_id)?.label ?? "Display"} · ${online ? inputState(view, profile.deviceId, null) : "Input state unknown"}`;
  const others = tasks.length === 0 ? null
    : tasks.length === 1 && !display ? `${task ? "Also running" : "Running"} · ${tasks[0]!.title}`
    : `${tasks.length} ${task ? "more " : ""}${tasks.length === 1 ? "task" : "tasks"} running`;
  const Icon = tasks.length > 0 ? Square : view?.desktop_lock === "locked" ? LockKeyhole : Pause;
  const text = [display, others].filter(Boolean).join(" · ");
  return (
    <button type="button" className={`chat-activity${tasks.length > 0 ? " chat-activity--stop" : ""}`} onClick={() => open.set(true)}
      aria-label={`${text}. Show Stop and input controls`}>
      <Icon className="chat-activity__icon" aria-hidden />
      <span className="chat-activity__text">{text}</span>
      <ChevronRight className="chat-activity__chev" aria-hidden />
    </button>
  );
}
