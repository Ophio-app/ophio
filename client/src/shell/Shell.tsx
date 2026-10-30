// SPDX-License-Identifier: Apache-2.0
//
// The everyday shell: five destinations, each with its own small stack.
// Every visited destination stays mounted, so moving between them keeps
// scroll, selection, drafts and open streams' state.

import { useContext, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { Store, useStore } from "../state/store";
import { openAttention, tasksMovedAt } from "../state/projection";
import { useApp, useHost, useUi, shallowEqual } from "../app/context";
import { TABS, type Tab } from "../app/ui";
import { talkShows } from "../app/taskOnScreen";
import { setNavBack, sync } from "../design/back";
import { Toasts } from "../design/Toast";
import { UrgentSlot } from "../design/urgent";
import SafetyStrip from "../screens/control/SafetyStrip";
import { RouteView } from "../screens/routes";
import { ColdStartOffline } from "./ColdStartOffline";
import { BottomNav } from "./BottomNav";
import { RouteProvider } from "./Screen";
import { holdFrame } from "./frame";
import { ChatShown, Immersive, isFocused, RailOpen } from "./focus";
import { useKeyboardOpen } from "./keyboard";
import { TaskAlerts } from "./AlertsOffer";
import { AlertsSetup } from "./AlertsSetup";
import { LoginUseNotices } from "./LoginUseNotices";
import { Notice } from "../design/Notice";
import { Button } from "../design/Button";

const TAB_LABEL: Record<Tab, string> = { talk: "Chat", control: "Control", work: "Tasks", library: "Library", todo: "To-Do" };

export function Shell() {
  const urgent = useContext(UrgentSlot);
  const { ui, platform, session } = useApp();
  const storageError = useStore(ui.persistence, (s) => s);
  const storageRecovery = useStore(ui.recovery, (s) => s);
  const tab = useUi((s) => s.tab);
  const stacks = useUi((s) => s.stacks);
  const [visited, setVisited] = useState<ReadonlySet<Tab>>(() => new Set([tab]));
  useKeyboardOpen();
  useEffect(() => holdFrame(), []);

  useEffect(() => {
    setVisited((v) => (v.has(tab) ? v : new Set([...v, tab])));
  }, [tab]);

  useEffect(() => {
    const talk = () => talkShows(session.state.get(), ui.get());
    setNavBack({ canGoBack: () => ui.canGoBack(talk()), back: () => ui.back(talk()) });
    const off = ui.subscribe(sync);
    return () => {
      off();
      setNavBack(null);
    };
  }, [ui, session]);

  useDefaultProject();
  useFirstOpen();

  const front = stacks[tab].at(-1);
  const [rail] = useState(() => new Store(false));
  const [chatShown] = useState(() => new Store<{ taskId: string | null } | null>(null));
  const chat = useStore(chatShown, (s) => s);
  const [fullStore] = useState(() => new Store(false));
  // A display shown full screen, or a picture being marked, hides the system
  // bars too, until it isn’t.
  const full = useStore(fullStore, (s) => s) &&
    (front?.screen === "desktop" || front?.screen === "annotate");
  useEffect(() => {
    if (!full) return;
    platform.immersive?.(true);
    return () => platform.immersive?.(false);
  }, [full, platform]);
  const onTalk = front?.screen === "talk" && chat !== null;
  const talking = onTalk && chat.taskId !== null;
  // A single-purpose destination is the whole screen until it is closed, and
  // so is an open conversation, as a chat is.
  const focused = isFocused(front, stacks[tab].length - 1) || (talking && stacks[tab].length === 1) || full;
  // Talk has its own Stop beside the task's title, so the rail doesn't repeat
  // it; the display that task uses stays in the rail with Pause input.
  // Control's root shows each display's input itself; the rail keeps the
  // Stops.
  const except =
    (front?.screen === "desktop" || front?.screen === "windows")
      ? { targetId: front.targetId }
      : talking
        ? { taskId: chat.taskId!, keepDisplay: true }
        : front?.screen === "control"
          ? { displays: true }
          : undefined;
  // Talk sums the rail up in a row under its header, which opens it.
  // A full-screen display's overlay counts the rail's tasks and opens it.
  const stow = onTalk || full;

  return (
    <UrgentSlot.Provider value={urgent ?? SafetyStrip}>
      <RailOpen.Provider value={rail}>
      <ChatShown.Provider value={chatShown}>
      <Immersive.Provider value={fullStore}>
      <div className={`app${focused ? " app--focused" : ""}${full ? " app--full" : ""}`}>
        <TaskAlerts />
        <AlertsSetup />
        <LoginUseNotices />
        {storageError ? (
          <Notice className="app__notice" tone="stop" action={<Button size="sm" onClick={() => ui.persistNow()}>Retry saving chats</Button>}>
            {storageError}
          </Notice>
        ) : storageRecovery && <Notice className="app__notice" tone="warn">{storageRecovery}</Notice>}
        <main className="app__main">
          {TABS.filter((t) => visited.has(t)).map((t) => {
            const stack = stacks[t];
            const onTab = t === tab;
            return (
              <Kept key={t} as="section" className="tab" shown={onTab} label={TAB_LABEL[t]}>
                {stack.map((route, i) => {
                  const top = i === stack.length - 1;
                  return (
                    <Kept key={`${i}:${JSON.stringify(route)}`} className="route" shown={top} refocus heading={i > 0}>
                      <RouteProvider value={{ tab: t, route, depth: i, active: onTab && top }}>
                        <ColdStartOffline route={route}><RouteView route={route} /></ColdStartOffline>
                      </RouteProvider>
                    </Kept>
                  );
                })}
              </Kept>
            );
          })}
        </main>
        <div className="app__safety">
          <SafetyStrip except={except} open={rail} tuck stow={stow} />
        </div>
        {!focused && <BottomNav />}
        <Toasts />
      </div>
      </Immersive.Provider>
      </ChatShown.Provider>
      </RailOpen.Provider>
    </UrgentSlot.Provider>
  );
}

const LOADING = ".screen__loading";
const TEXT_ENTRY = "input, textarea, select, [contenteditable]:not([contenteditable='false'])";

/**
 * A mounted tab or destination. Hidden ones are not rendered, which drops
 * their scroll offsets and focus, so both are recorded here and put back when
 * it is shown again. A region that was scrolled to its end stays at its end.
 * Only a destination returned to by Back takes focus back, and never into a
 * text field, which would open the keyboard. A destination opened for the
 * first time starts from its title instead, so a screen reader says where the
 * person now is.
 */
function Kept({
  as: Tag = "div",
  className,
  shown,
  label,
  refocus = false,
  heading = false,
  children,
}: {
  as?: "div" | "section";
  className: string;
  shown: boolean;
  label?: string;
  refocus?: boolean;
  heading?: boolean;
  children: ReactNode;
}) {
  const ref = useRef<HTMLElement>(null);
  const offsets = useRef(new Map<Element, { left: number; top: number; atEnd: boolean }>());
  const focused = useRef<HTMLElement | null>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const onScroll = (e: Event) => {
      const n = e.target;
      if (!(n instanceof Element) || n.clientHeight === 0) return;
      offsets.current.set(n, { left: n.scrollLeft, top: n.scrollTop, atEnd: n.scrollHeight - n.scrollTop - n.clientHeight < 2 });
    };
    const onFocus = (e: FocusEvent) => {
      if (e.target instanceof HTMLElement) focused.current = e.target;
    };
    el.addEventListener("scroll", onScroll, { capture: true, passive: true });
    el.addEventListener("focusin", onFocus);
    return () => {
      el.removeEventListener("scroll", onScroll, { capture: true });
      el.removeEventListener("focusin", onFocus);
    };
  }, []);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!shown || !el) return;
    for (const [n, o] of offsets.current) {
      if (!n.isConnected || !el.contains(n)) {
        offsets.current.delete(n);
        continue;
      }
      n.scrollLeft = o.left;
      n.scrollTop = o.atEnd ? n.scrollHeight : o.top;
    }
    const f = focused.current;
    if (!refocus || el.contains(document.activeElement)) return;
    if (f?.isConnected && el.contains(f) && !f.matches(TEXT_ENTRY)) {
      f.focus({ preventScroll: true });
      return;
    }
    if (!heading) return;
    const focusTitle = () => {
      const title = el.querySelector<HTMLElement>(".bar__title, .screen__title");
      if (!title) return;
      title.tabIndex = -1;
      title.focus({ preventScroll: true });
    };
    if (!el.querySelector(LOADING)) return focusTitle();
    // A screen still loading has no title yet; take the real one when it arrives,
    // unless the person has already moved on.
    const loaded = new MutationObserver(() => {
      if (el.querySelector(LOADING)) return;
      loaded.disconnect();
      const now = document.activeElement;
      if (!now || now === document.body) focusTitle();
    });
    loaded.observe(el, { childList: true, subtree: true });
    return () => loaded.disconnect();
  }, [shown, refocus, heading]);

  return (
    <Tag ref={ref as never} className={`${className}${shown ? "" : " is-hidden"}`} inert={!shown} aria-label={label}>
      {children}
    </Tag>
  );
}

/** Keep a real project selected: the remembered one, else Home, else the most recently active. */
function useDefaultProject(): void {
  const { ui } = useApp();
  const projectId = useUi((s) => s.projectId);
  const ids = useHost(
    (s) => {
      const moved = tasksMovedAt(s);
      return Object.values(s.projects)
        .map((p) => ({
          id: p.project_id,
          at: Math.max(0, ...Object.values(s.tasks).filter((t) => t.project_id === p.project_id).map((t) => moved[t.task_id] ?? 0)),
        }))
        .sort((a, b) => b.at - a.at)
        .map((x) => x.id);
    },
    shallowEqual,
  );
  const home = useHost((s) => Object.values(s.projects).find((p) => p.home)?.project_id);
  const loaded = useHost((s) => s.host !== null);

  useEffect(() => {
    if (!loaded || ids.length === 0) return;
    if (projectId && ids.includes(projectId)) return;
    ui.selectProject(home ?? ids[0]!);
  }, [loaded, ids, home, projectId, ui]);
}

/** A first run opens on Tasks when something there already needs the person,
 * not on an empty Chat. */
function useFirstOpen(): void {
  const { ui } = useApp();
  const ready = useHost((s) => s.caughtUp);
  const needsYou = useHost((s) => openAttention(s).length > 0);
  useEffect(() => {
    if (ready) ui.openFirst(needsYou);
  }, [ready, needsYou, ui]);
}
