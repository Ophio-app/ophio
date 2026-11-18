// SPDX-License-Identifier: Apache-2.0
//
// The compact context at the top of every destination root: which project,
// which computer, and how current the picture is, and how many tasks run
// while the safety rail steps aside. Pushed screens use ScreenBar instead,
// which keeps the freshness dot and the count.

import { SplitControl } from "../app/SplitControl";
import { ArrowLeft, ChevronDown, KeyboardOff, Settings, X } from "lucide-react";
import { useId, useState, type ReactNode } from "react";
import { useApp, useHost, useLink, useUi } from "../app/context";
import { useStore } from "../state/store";
import { IconButton } from "../design/Button";
import { freshness } from "./freshness";
import { hideKeyboard, useKeyboardOpen, useNarrowLayout } from "./keyboard";
import { elsewhere, ProjectSheet } from "./ProjectSheet";
import { StatusSheet } from "./StatusSheet";
import { ComputerControl } from "./ComputerMenu";
import { useEveryProject } from "../app/everyProject";
import { useTalkAll } from "../app/talkScope";
import { RailCount } from "../screens/control/SafetyStrip";
import { useRailOpen } from "./focus";
import { useOnScreen, useRouteTab } from "./Screen";

/** `heading` makes the project's name the screen's heading, for a root whose
 * subject is the project itself, so its body need not name it again.
 * `actions` go before Settings. A narrow bar (a small phone, or large text)
 * shows the status as its dot alone while the link is live, keeping the
 * project's name whole. `inert` shows it without taking taps, as while its
 * screen loads. */
export function ContextHeader({ heading = false, actions, inert = false, conversations = false }: { heading?: boolean; actions?: ReactNode; inert?: boolean; conversations?: boolean }) {
  const { profile, ui, all: allComputers } = useApp();
  const rail = useRailOpen();
  // Only the header on screen counts, not one under a pushed screen.
  const active = useOnScreen();
  const nameId = useId();
  const projectId = useUi((s) => s.projectId);
  // The tab this header belongs to, which a kept tab under another isn't showing.
  const routeTab = useRouteTab();
  const shownTab = useUi((s) => s.tab);
  const onTodo = (routeTab ?? shownTab) === "todo";
  const onWork = (routeTab ?? shownTab) === "work";
  // With All computers, Tasks and To-Do show every project on every one.
  const everywhere = useEveryProject(routeTab);
  const todoAll = useUi((s) => s.todoAll);
  const chatAll = useTalkAll();
  const all = conversations ? chatAll : everywhere || (!allComputers && todoAll && onTodo);
  const project = useHost((s) => (projectId ? s.projects[projectId] : undefined));
  const [sheet, setSheet] = useState<"project" | null>(null);
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
        {/* The pill names the computer, unless it says All: then the
            project names the computer it is on. */}
        {allComputers && <span className="ctx__computer ctx__computer--all">{!conversations && everywhere ? "On every computer" : <>On <bdi>{profile.name}</bdi></>}</span>}
      </button>)}
      <div className="ctx__end">
        {active && <RailCount open={rail} />}
        {actions}
        {active && <SplitControl />}
        <IconButton icon={Settings} label="Settings" onClick={() => ui.push({ screen: "settings" })} />
        {kb && <IconButton icon={KeyboardOff} label="Hide keyboard" onClick={hideKeyboard} />}
        <ComputerControl compact={narrow} />
      </div>
      <ProjectSheet open={sheet === "project"} onClose={() => setSheet(null)} conversations={conversations} allowAll={conversations || onTodo || (!!allComputers && onWork)} />
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
  const { profile, profiles, session } = useApp();
  // With more than one computer paired, a screen inside one names it.
  const named = !quiet && profiles.length > 1;
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
      className={`status status--${f.tone}${compact && f.live && !named ? " status--compact" : ""}${named ? " status--named" : ""}`}
      onClick={onOpen}
      aria-haspopup="dialog"
      aria-label={`${profile.name}: ${f.label}. Connection details`}
    >
      <span className="status__dot" aria-hidden />
      {named && <bdi className="status__name">{profile.name}</bdi>}
      {(!compact || !f.live) && (!named || !f.live) && <span className="status__label">{f.label}</span>}
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
        <SplitControl focused />
        {rail && <RailCount open={open} />}
        <StatusButton compact onOpen={() => setStatus(true)} />
      </div>
      <StatusSheet open={status} onClose={() => setStatus(false)} />
    </header>
  );
}
