// SPDX-License-Identifier: Apache-2.0
//
// Seen in the app counts as handled. Once the person has seen an item, its
// notification has done its job: it leaves this phone's shade, and the
// watcher tells the computer, which clears it from the person's other
// devices. The item itself stays where it is until it is dealt with.
//
// Opening the item's own screen (its task, chat, receipt or request) counts
// at once. A row in a list (Needs you, Unread, All) counts only when the
// person is plainly looking at it: the app in front and unlocked, the person
// using it lately, and the row well on screen for a moment, not folded away
// in a group. A tablet left open on a list then doesn't silence the phone.

import { useEffect, useState, useSyncExternalStore } from "react";
import type { Platform } from "../host/link";
import { backgroundAlerts } from "./alerts";
import { appLock } from "./lock";
import type { HostSession } from "../host/session";

/** Every id already told, by computer, for as long as the app runs. */
const told = new Map<string, Set<string>>();
const acknowledging = new Set<string>();
let waiting = new Map<string, { platform: Platform; ids: string[]; session?: HostSession }>();
let flushing = false;

function flush(): void {
  flushing = false;
  const batch = waiting;
  waiting = new Map();
  for (const [profileId, { platform, ids, session }] of batch) {
    const watcher = backgroundAlerts(platform);
    watcher?.handled(profileId, ids);
    // Without Android's watcher, as on iPhone, a new version's notice is
    // put away on the computer itself, which clears it everywhere.
    if (!watcher && session?.link.get().state === "online")
      for (const id of ids) {
        const item = session.state.get().attention[id];
        const tag = `${profileId}/${id}`;
        if (item?.new_version && !item.task_id && item.resolved_at === null && !acknowledging.has(tag) && !told.get(profileId)?.has(id)) {
          acknowledging.add(tag);
          void session.command({ command: "acknowledge_attention", attention_id: id })
            .then(() => {
              const done = told.get(profileId) ?? new Set<string>();
              done.add(id);
              told.set(profileId, done);
            })
            .catch(() => {})
            .finally(() => acknowledging.delete(tag));
        }
      }
    const tags = ids.map((id) => `${profileId}/${id}`);
    for (let i = 0; i < tags.length; i += 256)
      void Promise.resolve(platform.cancelNotifications?.(tags.slice(i, i + 256))).catch(() => {});
  }
}

/** The person saw these items of `profileId`'s on screen. Each is told
 * once; ids from one moment go together. */
export function markHandled(platform: Platform, profileId: string, attentionIds: Iterable<string>, session?: HostSession): void {
  const done = told.get(profileId) ?? new Set<string>();
  told.set(profileId, done);
  const fresh = [...attentionIds].filter((id) => id && !done.has(id));
  if (!fresh.length) return;
  for (const id of fresh) {
    const item = session?.state.get().attention[id];
    // An iPhone has no background watcher: the computer must accept this
    // command before the notice counts as told, so another visit can retry.
    if (!session || backgroundAlerts(platform) || !item?.new_version || item.task_id || item.resolved_at !== null)
      done.add(id);
  }
  const entry = waiting.get(profileId) ?? { platform, ids: [], session };
  entry.ids.push(...fresh);
  waiting.set(profileId, entry);
  if (!flushing) { flushing = true; void Promise.resolve().then(flush); }
}

/** Whether an id was already told as handled in this app session. */
export const wasHandled = (profileId: string, attentionId: string): boolean => !!told.get(profileId)?.has(attentionId);

/** For tests: forgets what was told, and that the person used the app. */
export function forgetHandled(): void { told.clear(); acknowledging.clear(); waiting.clear(); lastUsed = -Infinity; }

const subscribeVisibility = (listener: () => void) => {
  document.addEventListener("visibilitychange", listener);
  return () => document.removeEventListener("visibilitychange", listener);
};
const visible = () => document.visibilityState !== "hidden";

/** Whether the person can see the app's screen: in front and unlocked. */
export function useScreenSeen(platform: Platform): boolean {
  const lock = appLock(platform);
  const shown = useSyncExternalStore(subscribeVisibility, visible);
  const locked = useSyncExternalStore(lock.subscribe, () => lock.get().locked);
  return shown && !locked;
}

/**
 * While `attentionIds` are on the screen the person can see, they count as
 * handled. `active` is whether this screen is the one in front.
 */
export function useHandledOnScreen(platform: Platform, profileId: string, attentionIds: readonly string[], active: boolean, session?: HostSession): void {
  const seen = useScreenSeen(platform);
  const key = attentionIds.join(" ");
  useEffect(() => {
    if (active && seen && key) markHandled(platform, profileId, key.split(" "), session);
  }, [platform, profileId, key, active, seen, session]);
  useEffect(() => {
    if (!active || !seen || !key || !session || backgroundAlerts(platform)) return;
    const retry = () => {
      if (session.link.get().state === "online") markHandled(platform, profileId, key.split(" "), session);
    };
    return session.link.subscribe(retry);
  }, [platform, profileId, key, active, seen, session]);
}

/** How lately the person must have used the app for a row to count as seen. */
export const SEEN_RECENT_USE_MS = 60_000;
/** How long a row must stay on screen, without a break, to count as seen. */
export const SEEN_DWELL_MS = 2_000;
/** How much of a row must be on screen for it to count as showing. */
export const SEEN_VISIBLE_RATIO = 0.5;

// What the person does in the app: a touch, a click or a key. Scrolling by
// hand starts with a touch, the wheel or a key, so those count for it; a
// scroll event itself isn't counted, as the app also scrolls by itself.
const USE_EVENTS = ["pointerdown", "touchstart", "click", "keydown", "wheel"] as const;
let lastUsed = -Infinity;
const onUse = new Set<() => void>();
function used(): void {
  lastUsed = Date.now();
  for (const listener of [...onUse]) listener();
}
// From the start, so the tap that opened a list counts for it.
if (typeof window !== "undefined")
  for (const type of USE_EVENTS) window.addEventListener(type, used, { capture: true, passive: true });
const usedLately = () => Date.now() - lastUsed <= SEEN_RECENT_USE_MS;

/** Whether `el` is folded away: in a closed group (outside its summary) or
 * hidden. Not on screen, whatever the observer says. */
function folded(el: Element): boolean {
  for (let at: Element | null = el; at; at = at.parentElement) {
    if (at instanceof HTMLElement && at.hidden) return true;
    const up = at.parentElement;
    if (up instanceof HTMLDetailsElement && !up.open && at.tagName !== "SUMMARY") return true;
  }
  return false;
}

/**
 * A list's row of `attentionIds` counts as handled once the person has
 * looked at it: `active` (its screen is the one in front), the app in front
 * and unlocked, used in the last minute, and the row at least half on
 * screen for two seconds running, not folded away. Leaving any of these
 * starts the two seconds over. Returns the ref for the row's element; a row
 * not rendered is never seen.
 */
export function useHandledWhenSeen(platform: Platform, profileId: string, attentionIds: readonly string[], active: boolean): (el: Element | null) => void {
  const [el, setEl] = useState<Element | null>(null);
  const seen = useScreenSeen(platform);
  const key = attentionIds.join(" ");
  useEffect(() => {
    if (!el || !active || !seen || !key || typeof IntersectionObserver === "undefined") return;
    const row = el;
    const ids = key.split(" ");
    if (ids.every((id) => wasHandled(profileId, id))) return;
    let showing = false;
    let dwelt = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const stop = () => {
      clearTimeout(timer);
      observer.disconnect();
      onUse.delete(report);
      document.removeEventListener("toggle", update, true);
    };
    // Looked at long enough; it counts once the person is using the app.
    function report() {
      if (!dwelt || !usedLately()) return;
      stop();
      markHandled(platform, profileId, ids);
    }
    function update() {
      const now = showing && !folded(row);
      if (now && timer === undefined && !dwelt) timer = setTimeout(() => { dwelt = true; report(); }, SEEN_DWELL_MS);
      if (!now) { clearTimeout(timer); timer = undefined; dwelt = false; }
    }
    const observer = new IntersectionObserver((entries) => {
      const last = entries.at(-1);
      if (last) showing = last.isIntersecting && last.intersectionRatio >= SEEN_VISIBLE_RATIO;
      update();
    }, { threshold: SEEN_VISIBLE_RATIO });
    observer.observe(el);
    onUse.add(report);
    // A group opening or closing; the observer may not say, as its rows'
    // boxes come and go.
    document.addEventListener("toggle", update, true);
    return stop;
  }, [el, platform, profileId, key, active, seen]);
  return setEl;
}
