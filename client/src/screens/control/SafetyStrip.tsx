// SPDX-License-Identifier: Apache-2.0
import { ChevronDown, Hand, LockKeyhole, LogOut, Pause, Square } from "lucide-react";
import {
  useApp,
  useCommand,
  useHost,
  useLink,
  shallowEqual,
  useUi,
} from "../../app/context";
import { motionReduced } from "../../app/prefs";
import { useTaskOnScreen } from "../../app/taskOnScreen";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Button } from "../../design/Button";
import { announce } from "../../design/Announcer";
import { useBackLayer } from "../../design/back";
import type { ControlTarget, Task, TargetControlView } from "../../protocol/types";
import { releaseHuman } from "./useControl";
import { isTaskRunning, runningTasks } from "../../state/projection";
import { Store, useStore } from "../../state/store";
import { StopTask } from "./StopTask";
import { STOP_WAIT_MS, stopStore } from "./stops";
import { useLeaving, type Leaving } from "./leaving";
import { receiptWindow } from "./receiptWindow";
import { holdsDisplay, taskDisplayId, taskDisplayLabel } from "./taskDisplay";
import { handBackTarget } from "../../state/handBack";
import { GiveInputBack } from "../talk/HandBack";
import { useOpenTask } from "../work/shared";
import "./control.css";

export function SafetyActions({
  target,
  task,
  canTakeOver = false,
  onTakeOver,
  onLeave,
  human = false,
  takingOver = false,
  leaving = null,
  compact = false,
  describedBy,
}: {
  target?: ControlTarget;
  task?: Task;
  canTakeOver?: boolean;
  onTakeOver?: () => void;
  onLeave?: () => void;
  human?: boolean;
  takingOver?: boolean;
  leaving?: Leaving;
  /** Omit Pause once input is already paused; the status says so. Where
   * Resume agent shows, it takes Pause's place everywhere. */
  compact?: boolean;
  /** The element naming the task, so Stop says which one it ends. */
  describedBy?: string;
}) {
  const { session } = useApp();
  const run = useCommand();
  const online = useLink().state === "online";
  const grants = useHost((s) => s.grants);
  const view = useHost((s) =>
    target ? s.control[target.target_id] : undefined,
  );
  const waiting = useHost((s) =>
    target ? Object.values(s.tasks).find((t) => t.target_id === target.target_id && !!handBackTarget(s, t)) : undefined,
  );
  const receiptAt = useHost(
    (s) =>
      s.interventions
        .filter(
          (r) =>
            r.targetId === target?.target_id ||
            (task && r.taskId === task.task_id),
        )
        .at(-1)?.at ?? 0,
  );
  const requested = useStore(session.commands, (all) =>
    Object.values(all)
      .filter(
        (c) =>
          c.issuedAt > receiptAt &&
          ["sending", "accepted", "unknown"].includes(c.phase) &&
          ((c.body.command === "pause_input" &&
            c.body.target_id === target?.target_id) ||
            (c.body.command === "stop_task" &&
              c.body.task_id === task?.task_id)),
      )
      .at(-1),
  );
  return (
    <div className="safety-actions" aria-label="Safety controls">
      {human && onLeave ? (
        <Button
          className="intervention"
          variant="stop-outline"
          icon={LogOut}
          onClick={onLeave}
          disabled={leaving === "pending"}
        >
          {leaving === "pending" ? "Leaving control…" : leaving === "unknown" ? "Retry Leave control" : "Leave control"}
        </Button>
      ) : (
        <>
          {waiting && <GiveInputBack task={waiting} />}
          {!waiting && !(compact && view?.input_state === "paused") && (
            <Button
              className="intervention"
              variant="warn"
              icon={Pause}
              disabled={
                !online ||
                !target ||
                !grants?.control_input ||
                view?.input_state === "paused" ||
                view?.input_state === "ready"
              }
              onClick={() => {
                if (target) {
                  announce("Pause requested");
                  void run(
                    { command: "pause_input", target_id: target.target_id },
                    { label: "Pause requested" },
                  );
                }
              }}
            >
              {requested?.body.command === "pause_input" ? (
                requested.phase === "unknown" ? (
                  "Pause unconfirmed"
                ) : (
                  "Pause requested"
                )
              ) : (
                <>
                  Pause<span className="intervention__more"> agent</span>
                </>
              )}
            </Button>
          )}
          {onTakeOver && (
            <Button
              className="intervention"
              icon={Hand}
              disabled={!online || !canTakeOver || !grants?.control_input}
              onClick={onTakeOver}
            >
              {takingOver ? "Taking over…" : "Take over"}
            </Button>
          )}
        </>
      )}
      {task && <StopTask task={task} describedBy={describedBy} />}
    </div>
  );
}

/** `keepDisplay` keeps the excepted task's display, and its input controls,
 * in the rail while the screen shows only that task's Stop. `displays`
 * leaves every display to a screen that shows each one's input; this
 * device's own control still keeps Leave control in the rail. */
type Except = { targetId?: string; taskId?: string; keepDisplay?: boolean; displays?: boolean };

/** What the rail holds: each running task's Stop, and the displays whose
 * input someone or something holds or that need checking. */
export function useRail(except?: Except) {
  const { session, profile } = useApp();
  const running = useHost(runningTasks, shallowEqual);
  const stops = useStore(stopStore(session), (s) => s);
  const selected = useTaskOnScreen()?.task_id;
  const focusedTarget = useUi((s) => {
    const front = s.stacks[s.tab].at(-1);
    return front?.screen === "desktop" ? front.targetId : null;
  });
  const targets = useHost((s) => s.targets);
  const controls = useHost((s) => Object.values(s.control), shallowEqual);
  const included = running.filter((t) => t.task_id !== except?.taskId && !(stops[t.task_id]?.phase === "not_running" && stops[t.task_id]?.runId === t.current_run));
  const excluded = running.find(t => t.task_id === except?.taskId);
  const views = controls.filter((v) => v.target_id !== except?.targetId && !except?.displays &&
    (!excluded || except?.keepDisplay || taskDisplayId(excluded, controls) !== v.target_id || included.some(t => taskDisplayId(t, controls) === v.target_id)) &&
    (v.lease !== null || v.input_state === "unknown" || v.desktop_lock === "locked" ||
      included.some(t => taskDisplayId(t, controls) === v.target_id)));
  const onFront = (t: Task) => t.task_id === selected || (focusedTarget !== null && t.target_id === focusedTarget);
  const relevant = [...views].sort((a, b) => inputPriority(a) - inputPriority(b));
  const holdsFirst = (t: Task) => !!relevant[0] && holdsDisplay(t, relevant[0]);
  const tasks = included
    .sort((a, b) => Number(onFront(b)) - Number(onFront(a)) || Number(holdsFirst(b)) - Number(holdsFirst(a)) ||
      Number(!!taskDisplayId(b, controls)) - Number(!!taskDisplayId(a, controls)));
  const human = (except?.displays ? controls : views).find((v) => v.lease?.state === "active" && v.lease.owner.kind === "human" && v.lease.owner.device_id === profile.deviceId);
  // Input someone may have to take back right now: an agent holds or is
  // giving up a display, a running task works on one, or this device has it.
  // A locked or unconfirmed display nobody is driving can wait a tap.
  const live = relevant.some((v) => v === human || v.input_state === "agent" || v.input_state === "revoking" ||
    (v.lease?.owner.kind === "agent" && v.lease.state !== "revoked" && v.lease.state !== "expired") ||
    tasks.some((t) => holdsDisplay(t, v)));
  return { tasks, relevant, live, human, targets, controls, focusedTarget };
}

/** One compact rail, including inside a modal’s non-inert urgent slot. */
export default function SafetyStrip({
  except,
  dismiss,
  open,
  tuck = false,
  stow = false,
}: {
  except?: Except;
  dismiss?: () => void;
  /** Whether the rail is expanded, when something else can open it too. */
  open?: Store<boolean>;
  /** The rail steps aside while no agent or person anywhere holds a
   * display's input, and the screen's header counts the running tasks and
   * opens it. A sheet's copy of the rail never does, so Stop stays in reach. */
  tuck?: boolean;
  /** The screen sums the rail up in a row of its own, which opens it; it
   * shows only while open. */
  stow?: boolean;
}) {
  const { session, profile, ui } = useApp();
  receiptWindow(session);
  const [own] = useState(() => new Store(false));
  const rail = open ?? own;
  const expanded = useStore(rail, (s) => s);
  const setExpanded = (next: boolean) => rail.set(next);
  const content = useRef<HTMLDivElement>(null);
  const [more, setMore] = useState(false);
  useLayoutEffect(() => {
    const el = content.current;
    if (!el || !expanded) return;
    const check = () => setMore(el.scrollHeight > el.clientHeight + el.scrollTop + 2);
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(check);
    observer?.observe(el);
    for (const child of el.children) observer?.observe(child);
    el.addEventListener("scroll", check);
    window.addEventListener("resize", check);
    check();
    return () => { observer?.disconnect(); el.removeEventListener("scroll", check); window.removeEventListener("resize", check); };
  }, [expanded]);
  const voice = useUi((s) => s.stacks[s.tab].at(-1)?.screen === "voice");
  const { tasks, relevant, human, targets, controls, focusedTarget } = useRail(except);
  // Live anywhere, not only in what this screen leaves to the rail, as the
  // header's count shows only while nothing is: one of the two is always up.
  const live = useRail().live;
  const leaving = useLeaving(session, human?.lease);
  const online = useLink().state === "online";
  const stopped = useStopped(except?.taskId);
  const openTask = useOpenTask();
  const all = expanded;
  const shown = tasks.length > 0 || relevant.length > 0 || (all && stopped.length > 0);
  // Expanded, the rail covers the page, so Back collapses it before anything
  // else. The page stays readable behind it; the rail is not a modal.
  useBackLayer(all && shown, () => setExpanded(false));
  if (!shown || (tuck && !all && !live) || (stow && !all)) return null;
  const count = `${tasks.length} running ${tasks.length === 1 ? "task" : "tasks"}`;
  // A display's state is captioned above the Stop of the task holding it.
  // When the first Stop is another task's, the caption names the holder, so
  // the state never reads as that Stop's; expanding shows each one.
  const lead = tasks[0];
  const holder = lead && relevant.some((v) => holdsDisplay(lead, v)) ? lead
    : tasks.find((t) => relevant.some((v) => holdsDisplay(t, v)));
  const described = !lead ? relevant : holder ? relevant.filter((v) => holdsDisplay(holder, v)) : [];
  const describe = (v: TargetControlView) => `${targets.find((t) => t.target_id === v.target_id)?.label ?? "Display"}${holder && holder !== lead ? ` held by ${holder.title}` : ""} · ${inputState(v, profile.deviceId, leaving)}`;
  const caption = !online ? "Input state unknown" : described[0] ? describe(described[0]) : count;
  const paused = described.find((v) => v.desktop_lock === "locked")
    ?? described.find((v) => v.input_state === "paused" || v.input_state === "unknown");
  const also = online && paused && described[0] !== paused ? describe(paused) : null;
  // Collapsed, a paused or locked display shows under its holder's name, so
  // one held by a task behind the lead marks the count instead.
  const unseen = !all && online && holder !== lead ? paused : undefined;
  const StateIcon = unseen?.desktop_lock === "locked" ? LockKeyhole : Pause;
  return <aside className={`safety-rail${voice ? " safety-rail--voice" : ""}${all ? " safety-rail--expanded" : ""}`} aria-label="Safety controls">
    <button type="button" className="safety-rail__toggle" aria-label={`Safety · ${caption}${also ? ` · ${also}` : ""}${caption === count ? "" : ` · ${count}`}${all ? ". Close" : ""}`} aria-expanded={expanded} onClick={() => setExpanded(!expanded)}>
      <ChevronDown aria-hidden /><span className="safety-rail__caption">{all ? `Safety · ${count}` : caption}</span>{tasks.length > 0 && !all && <span className={`safety-rail__count num${unseen ? " safety-rail__count--warn" : ""}`} aria-hidden>{unseen && <StateIcon />}{tasks.length}</span>}
      {all && <span className="safety-rail__close">Close</span>}
    </button>
    <div className="safety-rail__content" ref={content}>
    {/* Expanded, the rail covers the page; say what it is and the way back. */}
    {all && <p className="safety-rail__intro">Stop a task, or pause an agent on one of this computer’s screens. Close or Back returns to your page.</p>}
    <div className="safety-rail__stops">
      {!all && !lead && relevant.length > 0 && <p className="safety-rail__summary">{caption}</p>}
      {(all ? tasks : tasks.slice(0, 1)).map((task) => {
        const label = taskDisplayLabel(task, controls, targets, online, focusedTarget);
        // Collapsed, the rail is one row: the lead task's name over the state
        // of the display it holds, a paused or locked one in the warning
        // tone, then Stop. Expanded, each task says where it works.
        const view = online && !all && holder === task ? paused ?? described[0] : undefined;
        const state = view ? `${inputState(view, profile.deviceId, leaving)} · ${targets.find((t) => t.target_id === view.target_id)?.label ?? "Display"}` : label;
        // The stop symbol alone; which task it ends is quiet text beside it,
        // and its accessible name says both.
        const stop = <StopTask key={task.task_id} task={task} displayLabel={label} iconOnly className="safety-rail__stop"
          caption={<><span className="safety-rail__title">{task.title}</span>{all
            ? label && <span className="safety-rail__where"> · {label}</span>
            : state && <span className={`safety-rail__line${view && view === paused ? " safety-rail__line--warn" : ""}`}>{state}</span>}</>} />;
        return all ? <div key={task.task_id} className="safety-rail__task">
          {stop}
          <Button variant="quiet" onClick={() => { setExpanded(false); dismiss?.(); openTask(task); }}>
            Open task{" "}<span className="visually-hidden">{task.title}</span>
          </Button>
        </div> : stop;
      })}
      {all && stopped.map(({ task, outcome }) => <p key={task.task_id} className="stop-status" role="status">
        Stopped · {task.title} · {outcome ?? "Waiting for the computer’s receipt…"}
      </p>)}
    </div>
    {online && human?.lease && <Button variant="stop-outline" className="intervention" icon={LogOut} disabled={leaving === "pending"} onClick={() => {
      if (human.lease) releaseHuman(session, human.lease);
    }}>{leaving === "pending" ? "Leaving control…" : leaving === "unknown" ? "Retry Leave control" : "Leave control"}</Button>}
    {online && all && relevant.map((view) => {
      const target = targets.find((t) => t.target_id === view.target_id);
      return <div key={view.target_id} className="safety-rail__target">
        <p>{target?.label ?? "Display unavailable"} · {inputState(view, profile.deviceId, leaving)}</p>
        {!human && <SafetyActions target={target} compact />}
        <Button variant="quiet" onClick={() => { dismiss?.(); ui.push({ screen: "desktop", targetId: view.target_id }, "control"); }}>Open control</Button>
      </div>;
    })}
    </div>
    {all && more && <Button className="safety-rail__more" variant="quiet" icon={ChevronDown} onClick={() => content.current?.scrollBy({ top: content.current.clientHeight * 0.7, behavior: motionReduced() ? "auto" : "smooth" })}>More controls below</Button>}
  </aside>;
}
export { SafetyStrip };

const CLOSED = new Store(false);

/** In a focused screen's bar while the rail is tucked: how many tasks are
 * running, and one tap to the rail with their Stops. With none running, a
 * locked or unconfirmed display still shows here, one tap from its controls. */
export function RailCount({ open }: { open: Store<boolean> | null }) {
  const { profile } = useApp();
  const { tasks, relevant, live, targets } = useRail();
  const online = useLink().state === "online";
  const expanded = useStore(open ?? CLOSED, (s) => s);
  if (!open || live || (tasks.length === 0 && relevant.length === 0)) return null;
  const toggle = () => open.set(!expanded);
  if (tasks.length === 0) {
    const view = relevant[0]!;
    // Nobody holds it, so no lease of this device's is being left.
    const state = `${targets.find((t) => t.target_id === view.target_id)?.label ?? "Display"} · ${online ? inputState(view, profile.deviceId, null) : "Input state unknown"}`;
    const more = relevant.length > 1 ? ` · ${relevant.length} displays` : "";
    const Icon = view.desktop_lock === "locked" ? LockKeyhole : Pause;
    return <button type="button" className="rail-count rail-count--input" aria-expanded={expanded} aria-label={`Safety · ${state}${more}. Show input controls`}
      title={state} onClick={toggle}>
      <Icon className="rail-count__state" aria-hidden />
    </button>;
  }
  const count = `${tasks.length} running ${tasks.length === 1 ? "task" : "tasks"}`;
  return <button type="button" className="rail-count" aria-expanded={expanded} aria-label={`${count}. Show Stop`} title={count}
    onClick={toggle}>
    <Square className="rail-count__icon" aria-hidden /><span className="num">{tasks.length}</span>
  </button>;
}

/** How long a stopped task's row stays once the computer says what ended. */
const STOPPED_MS = 10_000;

/** A stopped task keeps a row until the computer's receipt says what ended,
 * then shows that receipt briefly and lets it go. Unlike other safety
 * receipts, it outlasts the task and control changes the Stop itself causes. */
type StoppedRow = { task: Task; outcome: string | null; until: number };
function useStopped(except?: string): StoppedRow[] {
  const { session } = useApp();
  const asked = useStore(session.commands, (all) => Object.values(all).filter((c) => c.body.command === "stop_task"), shallowEqual);
  const [now, setNow] = useState(Date.now);
  const rows = useHost((s) => Object.values(s.tasks).flatMap((task): StoppedRow[] => {
    if (task.task_id === except || isTaskRunning(s, task)) return [];
    const record = s.interventions.findLast((r) => r.taskId === task.task_id && r.receipt.kind === "stop_task");
    if (record && now - record.at < STOPPED_MS) return [{ task, outcome: record.receipt.summary, until: record.at + STOPPED_MS }];
    const command = asked.findLast((c) => c.body.command === "stop_task" && c.body.task_id === task.task_id && c.phase !== "rejected");
    if (!command || now - command.issuedAt >= STOP_WAIT_MS) return [];
    // The conversation keeps each Stop's receipt beside the run it stopped.
    const answered = s.timelines[task.task_id]?.items.some((i) => i.run_id === task.current_run && i.item.kind === "receipt" && i.item.receipt.kind === "stop_task");
    return answered ? [] : [{ task, outcome: null, until: command.issuedAt + STOP_WAIT_MS }];
  }), (a, b) => a.length === b.length && a.every((row, i) => shallowEqual(row, b[i])));
  const next = Math.min(...rows.map((row) => row.until));
  useEffect(() => {
    if (!Number.isFinite(next)) return;
    const timer = setTimeout(() => setNow(Date.now()), Math.max(0, next - Date.now()));
    return () => clearTimeout(timer);
  }, [next]);
  return rows;
}

function inputPriority(view: TargetControlView): number {
  return view.input_state === "unknown" || view.input_state === "revoking" ? 0
    : view.input_state === "agent" || view.input_state === "human" ? 1 : 2;
}
export function inputState(view: TargetControlView, deviceId: string | null, leaving: Leaving): string {
  return view.desktop_lock === "locked" ? "Computer locked"
    : view.input_state === "agent" ? "Agent is in control"
    : view.input_state === "human" ? view.lease?.owner.kind === "human" && view.lease.owner.device_id === deviceId
      ? leaving ? "Leaving control…" : "This device has input" : "Another device has input"
    : view.input_state === "paused" ? "Agent paused" : view.input_state === "ready" ? "No one has input" : "Input needs checking";
}
