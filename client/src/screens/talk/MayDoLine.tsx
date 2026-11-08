// SPDX-License-Identifier: Apache-2.0
//
// The line saying whether this is a chat or a task and what it may do
// (`mayDo`), as quiet text that is a button. Tapping it opens what changes
// it: for a chat, or a task a chat was turned into, the access bar under the
// header, which takes focus and is outlined for a moment; for a task, a sheet
// with its mode, what that mode lets it do, and the way on to Work where there
// is one: Switch to Work in one tap for a Plan or Ask task, even mid-turn. Away from the task, the tap opens the task first. Where the computer
// can't hold a chat to a choice there is no bar, so the sheet says why.

import { Hammer, ListChecks } from "lucide-react";
import { useEffect, useState } from "react";
import { describeError, shallowEqual, useApp, useHost } from "../../app/context";
import { readOnlyLink } from "../../app/github";
import type { UiStore } from "../../app/ui";
import { Button } from "../../design/Button";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import type { Task } from "../../protocol/types";
import { Store, useStore } from "../../state/store";
import { accessHeld, describeAccess, heldAccess, normalizedAccess, unheldWords } from "./access";
import { finishedPlan, reviewPlan } from "./carryPlan";
import { governedByAccess, isChat, mayDo, type MayDoTask } from "./mayDo";
import { ModePicker } from "./ModePicker";
import { switchesToWork } from "./approvalWays";
import { offersSwitch, switchLabel } from "./SwitchToWorkChip";
import { otherHolder, switchToWork } from "./switchToWork";
import "./mayDo.css";

/** A request to open a task's controls, kept until its screen takes it. */
type Request = { taskId: string; at: number };
/** A request its task's screen didn't take this soon is dropped, so a task
 * opened much later doesn't jump to its controls. */
const STALE_MS = 10_000;

const requests = new WeakMap<UiStore, Store<Request | null>>();
function mayDoRequest(ui: UiStore): Store<Request | null> {
  let r = requests.get(ui);
  if (!r) { r = new Store<Request | null>(null); requests.set(ui, r); }
  return r;
}

/** Whether a task's access bar shows, which is where its controls are: the
 * same rule `AccessBar` shows by. */
export const accessBarShown = (task: Pick<Task, "conversation" | "access" | "state">, held: boolean): boolean =>
  held && governedByAccess(task) && task.state !== "archived";

/** Opens `task` in Talk, if it isn't the one shown, and then its controls. */
export function openMayDo(ui: UiStore, task: Pick<Task, "project_id" | "task_id">): void {
  const s = ui.get();
  const shown = s.tab === "talk" && !s.taskList && s.stacks.talk.length === 1
    && s.projectId === task.project_id && s.taskByProject[task.project_id] === task.task_id;
  if (!shown) ui.openTask(task.project_id, task.task_id);
  mayDoRequest(ui).set({ taskId: task.task_id, at: Date.now() });
}

/** Runs `take` when the controls of `taskId` are asked for and `mine`. */
export function useMayDoRequest(taskId: string | undefined, mine: boolean, take: () => void): void {
  const { ui } = useApp();
  const store = mayDoRequest(ui);
  const request = useStore(store, (s) => s);
  useEffect(() => {
    if (!mine || !taskId || request?.taskId !== taskId) return;
    store.set(null);
    if (Date.now() - request.at <= STALE_MS) take();
  }, [request, mine, taskId]);
}

/** The line for `task`, as the computer holds it now. */
export function useMayDo(task: MayDoTask): string {
  const held = useHost((s) => accessHeld(s.capabilities));
  const projects = useHost((s) => s.projects);
  return mayDo(task, held, projects);
}

/** The line, which opens what changes it; `onOpen` replaces that, as in a
 * list whose row opens the task. */
export function MayDoLine({ task, onOpen, className }: { task: Task; onOpen?: () => void; className?: string }) {
  const { ui } = useApp();
  const line = useMayDo(task);
  const held = useHost((s) => accessHeld(s.capabilities));
  const bar = accessBarShown(task, held);
  return (
    <button
      type="button"
      className={["may-do", className].filter(Boolean).join(" ")}
      onClick={onOpen ?? (() => openMayDo(ui, task))}
      aria-haspopup={!onOpen && !bar ? "dialog" : undefined}
      aria-label={`${line}. ${bar ? "Change what it may reach" : isChat(task) ? "What it may reach" : "About its mode"}`}
    >
      <span className="may-do__text">{line}</span>
    </button>
  );
}

/**
 * What the line opens where there is no access bar to open: a task's mode,
 * or why a chat's access can't be chosen here. Mounted once on the task's
 * screen, where it takes the line's requests.
 */
export function MayDoSheet({ task }: { task: Task }) {
  const held = useHost((s) => accessHeld(s.capabilities));
  const [open, setOpen] = useState(false);
  useMayDoRequest(task.task_id, !accessBarShown(task, held), () => setOpen(true));
  return open ? <MayDoDetail task={task} onClose={() => setOpen(false)} /> : null;
}

function MayDoDetail({ task, onClose }: { task: Task; onClose: () => void }) {
  const app = useApp();
  const line = useMayDo(task);
  const held = useHost((s) => accessHeld(s.capabilities));
  const caps = useHost((s) => s.capabilities);
  const projects = useHost((s) => s.projects);
  const harness = useHost((s) => s.harnesses.find((h) => h.kind === task.harness));
  const project = projects[task.project_id];
  const [busy, setBusy] = useState(false);
  const switches = useHost((s) => switchesToWork(s.capabilities));
  const holder = useHost((s) => otherHolder(s, task.project_id, task.task_id), shallowEqual);
  const chat = isChat(task);
  const mode = task.mode ?? "work";
  const agent = harness?.display_name ?? "the agent";
  const place = project?.name ?? "its project";

  const carry = finishedPlan(task) && !readOnlyLink(task.github?.link) ? async () => {
    setBusy(true);
    try {
      await reviewPlan(app, task);
      onClose();
    } catch (e) {
      toast(describeError(e, app.profile.name), { tone: "stop" });
    } finally {
      setBusy(false);
    }
  } : null;

  // A Plan or Ask task, or a chat, carries on in Work in one tap, its turn
  // stopped first if it is running.
  const tap = offersSwitch(task, switches) ? async () => {
    setBusy(true);
    try {
      await switchToWork(app.session, task, null);
      onClose();
      if (holder) toast(`Switched to Work. It waits in Next up while ${holder.title ? `“${holder.title}”` : "another task"} changes ${place}.`);
    } catch (e) {
      toast(describeError(e, app.profile.name), { tone: "stop" });
    } finally {
      setBusy(false);
    }
  } : null;
  const waits = holder ? ` While ${holder.title ? `“${holder.title}”` : "another task"} changes ${place}, it waits its turn in Next up.` : "";

  let body: string;
  if (governedByAccess(task)) {
    const access = chat ? heldAccess(task, held) : normalizedAccess(task.access!);
    const reach = project ? describeAccess(access, project, projects) : "";
    const why = chat && !held ? unheldWords(caps) : null;
    body = [reach, why].filter(Boolean).join(" ");
  } else if (mode === "work") {
    body = `It can change files and run commands in ${place}, as far as ${agent}’s own permissions allow.`;
  } else if (readOnlyLink(task.github?.link)) {
    body = "It only reads. What it reports is acted on by the issues and fixes it leads to.";
  } else if (tap) {
    body = `It only reads. Switch it to Work, and ${agent} carries on in the same session with what it has learned, able to change files and run commands in ${place}.${waits}`;
  } else if (mode === "plan") {
    body = carry
      ? "It only reads. To let it change files, carry out its plan: it carries on in Work mode."
      : "It only reads. Once it has proposed a plan, you can carry it out in Work mode, which can change files.";
  } else {
    body = "It only reads. To let it change files, start a new task in Work mode.";
  }

  return (
    <Sheet open onClose={onClose} title={chat ? "What this chat may do" : "What this task may do"} subtitle={line}>
      {!governedByAccess(task) && (
        <ModePicker harness={harness} value={mode} onChange={() => undefined} only={{ mode, why: tap ? "Switch it to Work below." : "A task keeps the mode it started in." }} />
      )}
      {body && <p className="may-do__detail">{body}</p>}
      {carry && <Button variant="primary" icon={ListChecks} pending={busy} onClick={() => void carry()}>Carry out this plan</Button>}
      {tap && <Button variant="primary" icon={Hammer} pending={busy} onClick={() => void tap()}>{switchLabel(task)}</Button>}
    </Sheet>
  );
}
