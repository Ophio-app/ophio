// SPDX-License-Identifier: Apache-2.0
//
// The compact context at the top of every destination root: which project,
// which computer, and how current the picture is, and how many tasks run
// while the safety rail steps aside. Pushed screens use ScreenBar instead,
// which keeps the freshness dot and the count.

import { ArrowLeft, ChevronDown, KeyboardOff, Settings, X } from "lucide-react";
import { useId, useState, type ReactNode } from "react";
import { useApp, useHost, useLink, useUi } from "../app/context";
import { useStore } from "../state/store";
import { IconButton } from "../design/Button";
import { freshness } from "./freshness";
import { hideKeyboard, useKeyboardOpen, useNarrowLayout } from "./keyboard";
import { elsewhere, ProjectSheet } from "./ProjectSheet";
import { StatusSheet } from "./StatusSheet";
import { RailCount } from "../screens/control/SafetyStrip";
import { useRailOpen } from "./focus";
import { useOnScreen } from "./Screen";

/** `heading` makes the project's name the screen's heading, for a root whose
 * subject is the project itself, so its body need not name it again.
 * `actions` go before Settings. A narrow bar (a small phone, or large text)
 * shows the status as its dot alone while the link is live, keeping the
 * project's name whole. `inert` shows it without taking taps, as while its
 * screen loads. */
export function ContextHeader({ heading = false, actions, inert = false }: { heading?: boolean; actions?: ReactNode; inert?: boolean }) {
  const { profile, ui } = useApp();
  const rail = useRailOpen();
  // Only the header on screen counts, not one under a pushed screen.
  const active = useOnScreen();
  const nameId = useId();
  const projectId = useUi((s) => s.projectId);
  const onTodo = useUi((s) => s.tab === "todo");
  const all = useUi((s) => s.todoAll) && onTodo;
  const project = useHost((s) => (projectId ? s.projects[projectId] : undefined));
  const [sheet, setSheet] = useState<"project" | "status" | null>(null);
  const kb = useKeyboardOpen();
  const waiting = useHost((s) => elsewhere(s, projectId));
  const narrow = useNarrowLayout();

  return (
    <header className="ctx" inert={inert}>
      {wrap(heading, nameId, <button type="button" className="ctx__context" onClick={() => setSheet("project")} aria-haspopup="dialog">
        <span className="ctx__project">
          <span className="ctx__project-name" id={nameId}>{all ? "All projects" : project?.name ?? "Choose a project"}</span>
          <ChevronDown className="ctx__chevron" aria-hidden />
          {waiting > 0 && !all && (
            <span className="ctx__elsewhere">
              <span aria-hidden />
              <span className="visually-hidden">{waiting} waiting in other projects</span>
            </span>
          )}
        </span>
        <span className="ctx__computer">{profile.name}</span>
      </button>)}
      <div className="ctx__end">
        {active && <RailCount open={rail} />}
        {actions}
        <IconButton icon={Settings} label="Settings" onClick={() => ui.push({ screen: "settings" })} />
        {kb && <IconButton icon={KeyboardOff} label="Hide keyboard" onClick={hideKeyboard} />}
        <StatusButton compact={narrow} onOpen={() => setSheet("status")} />
      </div>
      <ProjectSheet open={sheet === "project"} onClose={() => setSheet(null)} allowAll={onTodo} />
      <StatusSheet open={sheet === "status"} onClose={() => setSheet(null)} />
    </header>
  );
}

/** The heading is named by the project alone, not the computer under it. */
function wrap(heading: boolean, nameId: string, button: ReactNode) {
  return heading ? <h1 className="ctx__heading ctx__title" aria-labelledby={nameId}>{button}</h1> : button;
}

/** `quiet` shows nothing while the link is live, as a chat names its
 * connection only while there is trouble. */
export function StatusButton({ onOpen, compact, quiet }: { onOpen: () => void; compact?: boolean; quiet?: boolean }) {
  const { profile, session } = useApp();
  const link = useLink();
  const caughtUp = useHost((s) => s.caughtUp);
  const unanswered = useStore(session.unanswered, (s) => s);
  // Its label, tone and liveness follow the link and its answers alone, so it
  // neither ticks nor renders again each time the computer is heard; only the
  // detail it does not show says when that was.
  const f = freshness(link, caughtUp, null, 0, profile.name, unanswered);
  if (quiet && f.live) return null;
  return (
    <button
      type="button"
      className={`status status--${f.tone}${compact && f.live ? " status--compact" : ""}`}
      onClick={onOpen}
      aria-haspopup="dialog"
      aria-label={`${profile.name}: ${f.label}. Connection details`}
    >
      <span className="status__dot" aria-hidden />
      {(!compact || !f.live) && <span className="status__label">{f.label}</span>}
    </button>
  );
}

/**
 * Header for a pushed destination: Back, title and screen actions. A focused
 * destination opened over a tab closes with an X instead. While the
 * running-tasks rail is tucked away, every header counts them. One focused
 * screen opened from another still goes Back to it. The title is the
 * screen's h1 unless `heading` is off, for a body that has its own.
 */
export function ScreenBar({ title, heading = true, subtitle, actions, onBack, backLabel, close = false, rail = false }: {
  backLabel?: string; title: ReactNode; heading?: boolean; subtitle?: ReactNode; actions?: ReactNode; onBack?: () => void;
  close?: boolean;
  /** Show the running-tasks count while the rail is tucked. */
  rail?: boolean;
}) {
  const { ui } = useApp();
  const [status, setStatus] = useState(false);
  const kb = useKeyboardOpen();
  const open = useRailOpen();
  return (
    <header className={`bar${close ? " bar--close" : ""}`}>
      <IconButton icon={close ? X : ArrowLeft} label={backLabel ?? (close ? "Close" : "Back")} onClick={onBack ?? (() => ui.back())} />
      <div className="bar__titles">
        {heading ? <h1 className="bar__title">{title}</h1> : <p className="bar__title">{title}</p>}
        {subtitle && <p className="bar__subtitle">{subtitle}</p>}
      </div>
      <div className="bar__actions">
        {kb && <IconButton icon={KeyboardOff} label="Hide keyboard" onClick={hideKeyboard} />}
        {actions}
        {rail && <RailCount open={open} />}
        <StatusButton compact onOpen={() => setStatus(true)} />
      </div>
      <StatusSheet open={status} onClose={() => setStatus(false)} />
    </header>
  );
}
