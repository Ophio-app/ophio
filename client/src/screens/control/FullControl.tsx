// SPDX-License-Identifier: Apache-2.0
//
// A display shown full screen, with its controls drawn over the picture: see-
// through, so the desktop shows behind them, and one tap from out of the way.
// They sit in the room the picture leaves at its edges (D-055): the display and
// who has it at the top, the hands at one side, the view and Stop at the other.
// Hidden, a handle keeps Show controls and the running tasks' Stops in reach.

import { useState, useSyncExternalStore, type ReactNode } from "react";
import {
  ArrowLeft,
  EyeOff,
  Keyboard as KeyboardIcon,
  Mic,
  MousePointerClick,
  Move,
  Pointer,
  Scan,
  SlidersHorizontal,
  Smartphone,
  Square,
  Touchpad,
  ZoomIn,
  ZoomOut,
  type LucideIcon,
} from "lucide-react";
import { useApp } from "../../app/context";
import type { OwnershipInfo } from "../../design/Ownership";
import { useRailOpen } from "../../shell/focus";
import type { Task } from "../../protocol/types";
import type { PictureTools } from "./LivePicture";
import { useRail } from "./SafetyStrip";
import { StopTask } from "./StopTask";
import "./fullControl.css";

const wide = () => (typeof matchMedia === "function" ? matchMedia("(orientation: landscape)") : null);
/** Whether the phone is on its side, where a display is always shown full screen. */
export function useLandscape(): boolean {
  return useSyncExternalStore(
    (changed) => { const q = wide(); q?.addEventListener("change", changed); return () => q?.removeEventListener("change", changed); },
    () => wide()?.matches ?? false,
  );
}

export function Tool({ icon: Icon, label, short, pressed, disabled, onClick }: {
  icon: LucideIcon; label: string; short: string; pressed?: boolean; disabled?: boolean; onClick: () => void;
}) {
  return <button type="button" className={`full-tool${pressed ? " is-pressed" : ""}`} aria-label={label} title={label}
    aria-pressed={pressed} disabled={disabled} onClick={onClick}>
    <Icon aria-hidden /><span className="full-tool__label" aria-hidden>{short}</span>
  </button>;
}

/** The running tasks' Stops, one tap away in the rail; `except` leaves out a
 * task whose Stop is already on the overlay. */
export function RailButton({ except }: { except?: string }) {
  const rail = useRailOpen();
  const { tasks } = useRail();
  const others = tasks.filter((t) => t.task_id !== except).length;
  if (!rail || others === 0) return null;
  const count = `${others} ${except ? "other " : ""}running ${others === 1 ? "task" : "tasks"}`;
  return <button type="button" className="full-count" aria-label={`${count}. Show Stop`} title={count} onClick={() => rail.set(true)}>
    <Square aria-hidden /><span className="num">{others}</span>
  </button>;
}

export function FullControl({ tools, title, owner, task, safety, canInput, onKeyboard, onJev, voice, tilt, onTilt, notices }: {
  tools: PictureTools;
  title: string;
  owner: OwnershipInfo;
  /** The task driving this display, whose Stop stays on the overlay. */
  task?: Task;
  /** Leave control, Take over or Pause, as the display's state offers. */
  safety: ReactNode;
  canInput: boolean;
  onKeyboard?: () => void;
  /** Hands the display to Jev, when the computer offers voice control. */
  onJev?: () => void;
  /** Talk to voice control while it holds the display. */
  voice?: ReactNode;
  tilt: boolean;
  onTilt: () => void;
  notices: ReactNode;
}) {
  const { ui } = useApp();
  const [hidden, setHidden] = useState(false);
  const OwnerIcon = owner.icon;
  const status = <div className={`full-status full-status--${owner.tone}`}>
    <OwnerIcon aria-hidden className="full-status__icon" />
    <div className="full-status__text">
      <h1 className="bar__title full-status__title">{title}</h1>
      <p className="full-status__state" role="status">
        {owner.title}<span className={tools.fresh ? "" : "full-status__stale"}> · {tools.freshness}</span>
      </p>
    </div>
  </div>;
  const back = <button type="button" className="full-tool full-tool--round" aria-label="Back" title="Back" onClick={() => ui.back()}><ArrowLeft aria-hidden /></button>;
  const stop = task && <StopTask task={task} iconOnly status={false} className="full-stop" />;
  const hands = <>
    {voice}
    {onKeyboard && <Tool icon={KeyboardIcon} label="Keyboard" short="Keys" onClick={onKeyboard} />}
    {onJev && <Tool icon={Mic} label={`Let Jev control ${title}`} short="Jev" onClick={onJev} />}
  </>;
  const pointer = canInput && <>
    <Tool icon={tools.mode === "touchpad" ? Touchpad : Pointer} label={`Pointer: ${tools.mode}. Switch`}
      short={tools.mode === "touchpad" ? "Touchpad" : "Touch"} onClick={() => tools.setMode(tools.mode === "touchpad" ? "touch" : "touchpad")} />
    {/* Under Touch, Right click applies to the next click, and a finger drags. */}
    <Tool icon={MousePointerClick} label="Right click" short="Right" pressed={tools.mode === "touch" ? tools.rightNext : undefined} onClick={tools.rightClick} />
    {tools.mode === "touchpad" && <Tool icon={Move} label="Drag" short="Drag" pressed={tools.drag} onClick={tools.toggleDrag} />}
  </>;
  const view = <>
    <Tool icon={ZoomOut} label="Zoom out" short="Out" disabled={tools.zoom <= 1} onClick={tools.zoomOut} />
    <Tool icon={Scan} label="Fit picture" short="Fit" disabled={tools.zoom <= 1} onClick={tools.fit} />
    <Tool icon={ZoomIn} label="Zoom in" short="In" disabled={tools.zoom >= 4} onClick={tools.zoomIn} />
    <Tool icon={Smartphone} label="Tilt to pan" short="Tilt" pressed={tilt} onClick={onTilt} />
  </>;
  const hide = <Tool icon={EyeOff} label="Hide controls" short="Hide" onClick={() => setHidden(true)} />;
  const notes = <div className="full-notices">{notices}{tools.notice}</div>;

  if (hidden)
    return <div className="full-overlay full-overlay--hidden">
      {notes}
      <div className="full-handle" role="group" aria-label="Controls hidden">
        <RailButton />
        <button type="button" className="full-tool full-tool--round" aria-label="Show controls" title="Show controls"
          onClick={() => setHidden(false)}><SlidersHorizontal aria-hidden /></button>
      </div>
    </div>;

  return <div className="full-overlay">
    <div className="full-edge full-edge--top">{back}{status}</div>
    {notes}
    <div className="full-edge full-edge--start" role="group" aria-label="Pointer">{hands}{pointer}</div>
    <div className="full-edge full-edge--end" role="group" aria-label="Picture controls">
      <div className="full-edge__safety">{stop}{safety}<RailButton except={task?.task_id} /></div>
      {view}{hide}
    </div>
  </div>;
}
