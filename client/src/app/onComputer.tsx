// SPDX-License-Identifier: Apache-2.0
//
// A part of the screen that shows another computer's work, as All's lists
// do. Inside it, everything the existing rows and cards read and send goes
// to that computer's own session, never the one last on screen: an answer,
// an Allow or a tick reaches the computer the item came from. Going
// somewhere from it, as opening a task, switches to that computer first,
// through the usual leave-control check, and then goes there.

import { useMemo, type ReactNode } from "react";
import type { ComputerEntry } from "./fleet";
import { AppProvider, useApp, type App } from "./context";
import type { UiStore } from "./ui";
import { ReportComputer } from "./reportComputer";

/** What moves the screen: done on the item's computer once it is shown. */
const MOVES = new Set<keyof UiStore>([
  "selectTab", "push", "swap", "up", "reset", "back", "leaveTask", "showTaskList", "finish",
  "selectProject", "contextProject", "openChecklist", "selectTask", "newConversation", "openTask",
  "selectCreated", "selectTarget", "requestTakeover", "setTodoAll", "setTodoItem",
]);

/** Asks to show the computer, and once it is shown, does `act` there. */
type Visit = (act: (ui: UiStore) => void) => void;
interface Deferred { ui: UiStore; target: UiStore; visit: Visit; queue: ((ui: UiStore) => void)[] | null; bound: Map<PropertyKey, unknown> }
const deferred = new WeakMap<UiStore, Deferred>();
const byProxy = new WeakMap<UiStore, Deferred>();

/** Adds `act` to what one tap asks of the computer: every move made in the
 * same moment goes in one switch, with one leave step, in the order made. */
function enqueue(kept: Deferred, act: (ui: UiStore) => void): void {
  if (kept.queue) { kept.queue.push(act); return; }
  const queue = (kept.queue = [act]);
  queueMicrotask(() => {
    kept.queue = null;
    kept.visit((shown) => { for (const step of queue) step(shown); });
  });
}

/** `ui`, with each move sent through `visit` instead of done here. One per
 * store, and each of its functions one too, so what subscribes to it keeps
 * its subscription. */
/** Every move asks for the switch, a tap's and one made after it awaits an
 * answer alike. A move that follows no tap, as an effect keeping a choice
 * up to date, must not switch computers: it is made on the store itself,
 * through `storeOf`. */
export function deferringUi(ui: UiStore, visit: Visit): UiStore {
  const known = deferred.get(ui);
  if (known) { known.visit = visit; return known.ui; }
  const kept: Deferred = { ui, target: ui, visit, queue: null, bound: new Map() };
  kept.ui = new Proxy(ui, {
    get(target, key) {
      const value: unknown = Reflect.get(target, key, target);
      if (typeof value !== "function") return value;
      if (kept.bound.has(key)) return kept.bound.get(key);
      const fn = typeof key === "string" && MOVES.has(key as keyof UiStore)
        ? (...args: unknown[]) => enqueue(kept, (shown) => { (shown[key as keyof UiStore] as (...a: unknown[]) => unknown)(...args); })
        : (value as (...a: unknown[]) => unknown).bind(target);
      kept.bound.set(key, fn);
      return fn;
    },
  });
  deferred.set(ui, kept);
  byProxy.set(kept.ui, kept);
  return kept.ui;
}

/** Runs `then` once the moves asked of `ui` happen: at once for the
 * computer on screen, after the switch for another, and never if the
 * person stays where they are. */
export function whenShown(ui: UiStore, then: () => void): void {
  const kept = byProxy.get(ui);
  if (kept) enqueue(kept, then); else then();
}

/** The store itself, for a change that moves nothing on screen, as Talk
 * keeping a stopped chat in view. */
export function storeOf(ui: UiStore): UiStore {
  return byProxy.get(ui)?.target ?? ui;
}

/** The app as seen from `entry`'s computer, or null while it has no session. */
export function useAppOn(entry: ComputerEntry): App | null {
  const app = useApp();
  const { profile, session } = entry;
  return useMemo(() => {
    if (entry.current) return app;
    if (!session || !app.uiOf || !app.visit) return null;
    const id = profile.profileId;
    const visit = app.visit;
    const shownUi = app.ui;
    return {
      ...app, profile, session,
      ui: deferringUi(app.uiOf(id), (act) => {
        // A move lands on the tab it was made from, Tasks or To-Do, above
        // that tab's own root, which shows All, so Back returns to All.
        const tab = shownUi.get().tab;
        visit(id, (shown) => { if (tab === "work" || tab === "todo") shown.reset(tab); act(shown); });
      }),
      forget: () => app.forgetOther?.(id) ?? Promise.resolve(),
    };
  }, [app, entry.current, profile, session]);
}

/** Shows `children` as part of `entry`'s computer. */
export function OnComputer({ entry, children }: { entry: ComputerEntry; children: ReactNode }) {
  const app = useAppOn(entry);
  // A box-less element marking this computer's part, so a tap in it is known as its own.
  return app ? <AppProvider app={app}><ReportComputer.Provider value={entry.profile.profileId}>{children}</ReportComputer.Provider></AppProvider> : null;
}
