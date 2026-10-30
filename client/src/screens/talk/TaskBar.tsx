// SPDX-License-Identifier: Apache-2.0
//
// What Talk's chat header (TalkHeader.tsx) shares with the rest of the app:
// a task's one state word and the sheet of what else there is about the
// task itself. Talk's list of conversations is Conversations.tsx.

import { FileText, Images, Pencil, ReceiptText } from "lucide-react";
import { useApp } from "../../app/context";
import type { UiStore } from "../../app/ui";
import { Store } from "../../state/store";
import type { Tone } from "../../design/Chip";
import { ListRow } from "../../design/ListRow";
import { Sheet } from "../../design/Sheet";
import type { QueueView, Run, Task, TaskState } from "../../protocol/types";
import { decisionLabel, planProposed, reportLabel, runDoubts } from "../work/logic";
import { acceptedLabel, unstartedQueueItem } from "../work/queueStatus";

const TASK_STATE: Record<TaskState, { label: string; tone: Tone }> = {
  accepted: { label: "Waiting to start", tone: "neutral" },
  dispatching: { label: "Starting", tone: "accent" },
  working: { label: "Working", tone: "accent" },
  awaiting_decision: { label: "Needs you", tone: "warn" },
  interrupted: { label: "Stopped", tone: "neutral" },
  completed: { label: "Finished", tone: "ok" },
  failed: { label: "Failed", tone: "stop" },
  reconciliation_required: { label: "Outcome unknown", tone: "warn" },
  archived: { label: "Archived", tone: "neutral" },
};

const WATCHING: { label: string; tone: Tone } = { label: "Watching", tone: "neutral" };
const WAITING: { label: string; tone: Tone } = { label: "Waiting", tone: "neutral" };

const RUN_STATE: Record<Run["state"], { label: string; tone: Tone }> = {
  admitted: TASK_STATE.dispatching,
  dispatching: TASK_STATE.dispatching,
  working: TASK_STATE.working,
  awaiting_approval: TASK_STATE.awaiting_decision,
  awaiting_input: TASK_STATE.awaiting_decision,
  stopping: { label: "Stopping", tone: "warn" },
  interrupted: TASK_STATE.interrupted,
  completed: TASK_STATE.completed,
  failed: TASK_STATE.failed,
  reconciliation_required: TASK_STATE.reconciliation_required,
};

// Starting a task closes Review, and the control that started it goes with
// it. The new task's heading takes focus once Talk shows the task, so a
// screen reader says what started and how it stands.
const headingRequests = new WeakMap<UiStore, Store<string | null>>();
export function taskHeadingFocus(ui: UiStore): Store<string | null> {
  let request = headingRequests.get(ui);
  if (!request) { request = new Store<string | null>(null); headingRequests.set(ui, request); }
  return request;
}

/** What else there is about the open task, from the ⋯ in its header: its
 * title to change, what it showed and was sent, the instructions it was
 * given, and its record. */
export function TaskMoreSheet({ open, onClose, task, onMedia, onRename }: { open: boolean; onClose: () => void; task: Task; onMedia?: () => void; onRename: () => void }) {
  const { ui } = useApp();
  const go = (route: Parameters<UiStore["push"]>[0]) => { onClose(); ui.push(route); };
  const noun = task.conversation ? "conversation" : "task";
  return (
    <Sheet open={open} onClose={onClose} title={`This ${noun}`} subtitle={task.title}>
      <div className="group">
        <ListRow icon={Pencil} title="Rename" subtitle={`Give this ${noun} a title you’ll recognize`} chevron onClick={onRename} />
        {onMedia && <ListRow icon={Images} title="Pictures and videos" subtitle={`Everything shown and sent in this ${noun}`} chevron onClick={onMedia} />}
        <ListRow icon={FileText} title="Instructions" subtitle={`Discovered files and revisions for this ${noun}`} chevron onClick={() => go({ screen: "instructions", taskId: task.task_id })} />
        <ListRow icon={ReceiptText} title={`What happened in this ${noun}`} subtitle="Runs, changes, checks and stops" chevron onClick={() => go({ screen: "receipt", taskId: task.task_id })} />
      </div>
    </Sheet>
  );
}

/**
 * A task's one state word beside what it needs from the person. The computer
 * keeps reporting a task working while it waits on the person, such as for
 * input to be given back, so a task with a reason says Needs you instead. So
 * does a task Next up holds for the person's go-ahead: it starts on nothing else.
 */
export function taskRowBadge(task: Task, run: Run | undefined, queue: QueueView | undefined, reason: string | null): { label: string; tone: Tone } {
  const badge = taskBadge(task, run, queue);
  if (!reason) return badge;
  if (badge === TASK_STATE.working || badge === WATCHING || badge === WAITING) return TASK_STATE.awaiting_decision;
  return unstartedQueueItem(task, run ? [run] : [], queue)?.state.state === "awaiting_start" ? TASK_STATE.awaiting_decision : badge;
}

const NEEDS_A_LOOK = { label: "Finished, needs a look", tone: "warn" } as const;
export function taskBadge(task: Task, run?: Run, queue?: QueueView): { label: string; tone: Tone } {
  if (task.state === "archived") return TASK_STATE.archived;
  if (run?.task_id !== task.task_id) run = undefined;
  const label = decisionLabel(run);
  if (label) return { label, tone: "neutral" };
  // What the agent said its turn came to, where it has questions or work left.
  const reported = reportLabel(task, run);
  if (reported) return { label: reported, tone: "warn" };
  if (planProposed(task, run)) return { label: "Plan proposed", tone: "ok" };
  // A newer run can reach the phone before the task's current_run and state.
  if (run && run.run_id !== task.current_run) return RUN_STATE[run.state];
  // Between turns, an agent waiting on something it was asked to watch is
  // watching, or waiting for another agent's answer, and the project is free.
  if (task.wake && !["accepted", "dispatching", "working", "awaiting_decision"].includes(task.state)) return task.wake.answer ? WAITING : WATCHING;
  // Finished with a failed check, a helper running or an unknown is not done.
  if (task.state === "completed" && run && runDoubts(run).length) return NEEDS_A_LOOK;
  return task.state === "accepted" ? { ...TASK_STATE.accepted, label: acceptedLabel(task, run ? [run] : [], queue) } : TASK_STATE[task.state];
}
