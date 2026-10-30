// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../../brand";
import { useState, type ReactNode } from "react";
import { Sheet } from "../../design/Sheet";
import { Square } from "lucide-react";
import { shallowEqual, useApp, useCommand, useHost, useLink, useUi } from "../../app/context";
import { Button, IconButton } from "../../design/Button";
import { toast } from "../../design/Toast";
import type { Task } from "../../protocol/types";
import { isTaskRunning } from "../../state/projection";
import { useStore } from "../../state/store";
import { stopStore, stopFrequency } from "./stops";
import { pinStoppedConversation } from "../../app/taskOnScreen";
import { taskDisplayLabel } from "./taskDisplay";
import { useWarning } from "../../app/warnings";
import "./control.css";

export function useStopIntent(task: Task) {
  const { session } = useApp();
  const intent = useStore(stopStore(session), (s) => s[task.task_id]);
  return intent?.runId === task.current_run ? intent : undefined;
}

export function StopStatus({ task }: { task: Task }) {
  const { session } = useApp();
  const intent = useStore(stopStore(session), (s) => s[task.task_id]);
  const running = useHost((s) => isTaskRunning(s, task));
  if (!intent || intent.runId !== task.current_run || (!running && intent.phase !== "not_running")) return null;
  const delivered = intent.phase === "accepted" || intent.phase === "still_running";
  return <p className={`stop-status stop-status--${intent.phase}`} role="status">{intent.count ? `Stop ${delivered ? "sent" : "requested"} ${stopFrequency(intent.count)} · not confirmed. ` : ""}{intent.detail}</p>;
}
export function StopTask({ task, describedBy, status = true, className = "", compact = false, displayLabel, iconOnly = false, caption }: {
  task: Task; describedBy?: string; status?: boolean; className?: string; compact?: boolean; displayLabel?: string;
  /** Only the stop symbol shows; the accessible name still says which task
   * and display it ends, and the status below still says how it stands. */
  iconOnly?: boolean;
  /** Quiet text before an icon-only Stop, for sighted readers only. */
  caption?: ReactNode;
}) {
  const { session, profile, ui } = useApp();
  const [confirm, setConfirm] = useState(false);
  const warn = useWarning("task.force_stop");
  const execute = useCommand();
  const owned = useHost((s) => !!s.runs[task.current_run ?? ""]?.force_stoppable);
  const target = useHost((s) => s.targets.find(t => t.target_id === task.target_id));
  const control = useHost((s) => task.target_id ? s.control[task.target_id] : undefined);
  const inputAllowed = useHost((s) => s.grants?.control_input === true);
  const store = stopStore(session);
  const intent = useStore(store, (s) => s[task.task_id]);
  const current = intent?.runId === task.current_run ? intent : undefined;
  const allowed = useHost((s) => s.grants?.tasks === true);
  const running = useHost((s) => isTaskRunning(s, task));
  const link = useLink();
  const controls = useHost(s => Object.values(s.control), shallowEqual);
  const targets = useHost(s => s.targets);
  const focusedTarget = useUi(s => {
    const front = s.stacks[s.tab].at(-1);
    return front?.screen === "desktop" ? front.targetId : null;
  });
  displayLabel ??= taskDisplayLabel(task, controls, targets, link.state === "online", focusedTarget);
  const pin = () => pinStoppedConversation(session.state.get(), ui, task);
  if (!running || current?.phase === "not_running") return status ? <StopStatus task={task} /> : null;
  const pending = current?.phase === "sending" || current?.phase === "accepted";
  const retry = current && ["waiting", "unknown", "refused"].includes(current.phase);
  const again = current?.phase === "still_running";
  const label = again ? "Stop again" : retry ? "Retry Stop" : pending ? "Stop requested" : "Stop";
  const name = `${label} · ${task.title}${displayLabel ? ` · ${displayLabel}` : ""}`;
  const disabled = !allowed || pending || link.state === "failed";
  const request = () => { pin(); toast(`Stop requested for “${task.title}”`); store.request(task); };
  const force = () => { pin(); toast(`Force stop requested for “${task.title}”`); store.request(task, true); };
  return <div className={`named-stop${iconOnly ? " named-stop--icon" : ""} ${className}`}>
    {iconOnly ? <>
      {caption != null && <span className="named-stop__caption" aria-hidden>{caption}</span>}
      <IconButton icon={Square} variant="stop" size="lg" label={name} className="intervention named-stop__icon" aria-describedby={describedBy}
        disabled={disabled} onClick={request} />
    </> : <Button variant="stop" icon={Square} className="intervention named-stop__button" aria-describedby={describedBy} aria-label={name}
      disabled={disabled}
      onClick={request}>
      <span className="named-stop__action">{compact && label === "Stop" ? "Stop agent" : label}</span>{!compact && <> · <span className="named-stop__task">{task.title}</span></>}{displayLabel && <span className="named-stop__display"><span className="named-stop__sep"> · </span>{displayLabel}</span>}
    </Button>}
    {status && <StopStatus task={task} />}
    {again && <div className="work-actions">
      {owned ? <Button variant="stop-outline" disabled={!allowed || link.state !== "online"} onClick={() => { pin(); warn.ask(() => setConfirm(true), force); }}>Force stop</Button>
        : <p className="fine">Stop it in the agent’s terminal on {profile.name}. {PRODUCT_NAME} did not start its process.</p>}
      {target && control?.input_state === "agent" && <Button disabled={!inputAllowed || link.state !== "online"} onClick={() => void execute({ command: "pause_input", target_id: target.target_id }, { label: "Pause agent" })}>Pause agent on {target.label}</Button>}
      {owned && <Button variant="quiet" onClick={() => ui.push(target ? { screen: "desktop", targetId: target.target_id } : { screen: "control" }, "control")}>Open in Control</Button>}
    </div>}
    <Sheet open={confirm} onClose={() => setConfirm(false)} urgentExcept={{ taskId: task.task_id }} title={`Force stop · ${task.title}`} footer={<Button variant="stop" disabled={!allowed || link.state !== "online"} onClick={() => { warn.agreed(); setConfirm(false); force(); }}>Force stop on {profile.name}</Button>}>
      <p>Ends the agent’s process on {profile.name}. Files it already changed stay changed; anything it was still writing may be incomplete.</p>
      {warn.check}
    </Sheet>
  </div>;
}
