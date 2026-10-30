// SPDX-License-Identifier: Apache-2.0

import { BookOpen, ListChecks, ListTodo, MessageSquare, Monitor, type LucideIcon } from "lucide-react";
import { useApp, useHost, useLink, useUi } from "../app/context";
import { useTodoBadge } from "../app/checklist";
import { navigationInputState } from "../screens/control/ownership";
import { openAttention } from "../state/projection";
import { TABS, tabLabel, type Tab } from "../app/ui";

const ICON: Record<Tab, LucideIcon> = { talk: MessageSquare, control: Monitor, work: ListChecks, library: BookOpen, todo: ListTodo };

export function BottomNav() {
  const { ui } = useApp();
  const tab = useUi((s) => s.tab);
  const attention = useHost((s) => openAttention(s).length);
  const todo = useTodoBadge();
  // Someone holds input on a screen: keep that visible from every destination.
  const online = useLink().state === "online";
  const inputState = useHost(s => navigationInputState(s, online));

  return (
    <nav className="nav" aria-label="Main">
      {TABS.map((t) => {
        const label = tabLabel(t), Icon = ICON[t];
        const on = t === tab;
        const status = t === "todo" && todo > 0 ? `${todo} ${todo === 1 ? "item" : "items"} waiting on you`
          : t === "work" && attention > 0 ? `${attention} ${attention === 1 ? "needs" : "need"} attention`
          : t === "control" && inputState !== "idle" ? (inputState === "held" ? "input is held" : "input state unknown")
          : null;
        // The status is part of one name: read from a visually hidden span,
        // Chrome set it apart as a block and said "Work , 1".
        return (
          <button
            key={t}
            type="button"
            className={`nav__tab${on ? " is-on" : ""}`}
            aria-current={on ? "page" : undefined}
            aria-label={status ? `${label}, ${status}` : undefined}
            onClick={() => ui.selectTab(t)}
          >
            <span className="nav__pill">
              <Icon className="nav__icon" aria-hidden strokeWidth={on ? 2.3 : 2} />
              {t === "control" && inputState === "held" && <span className="nav__dot" aria-hidden />}
              {t === "work" && attention > 0 && <span className="nav__count" aria-hidden>{attention}</span>}
              {t === "todo" && todo > 0 && <span className="nav__count" aria-hidden>{todo}</span>}
            </span>
            <span className="nav__label">{label}</span>
          </button>
        );
      })}
    </nav>
  );
}
