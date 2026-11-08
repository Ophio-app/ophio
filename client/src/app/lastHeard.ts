// SPDX-License-Identifier: Apache-2.0
import { useEffect } from "react";
import { backgroundAlerts, type AlertsView } from "../platform/alerts";
import { Store, useStore } from "../state/store";
import { useOnScreen } from "../shell/Screen";
import { useApp, useHost } from "./context";

const NONE = new Store<AlertsView>({ state: null, pending: false, error: null });

/** When the watcher kept for alerts while the app is closed last heard from
 * each paired computer, by profile. Followed while `shown`, so screens that
 * stay mounted behind others only read the native state while they show the
 * time. Empty where the phone keeps no such watcher. */
export function useHeardInBackground(shown = true): Readonly<Record<string, number>> | undefined {
  const watcher = backgroundAlerts(useApp().platform);
  const visible = useOnScreen() && shown;
  useEffect(() => visible ? watcher?.follow() : undefined, [watcher, visible]);
  return useStore(watcher ?? NONE, (s) => s.state?.heard);
}

/** When the phone last heard from the computer on screen, by either
 * connection. The app's own goes quiet while it is closed, but the watcher
 * kept for alerts may have heard from the computer since. */
export function useLastHeard(shown = true): number | null {
  const { profileId } = useApp().profile;
  const own = useHost((s) => s.lastHeardAt);
  const background = useHeardInBackground(shown);
  return later(own, background?.[profileId]);
}

export function later(a: number | null | undefined, b: number | null | undefined): number | null {
  return a == null ? b ?? null : b == null ? a : Math.max(a, b);
}
