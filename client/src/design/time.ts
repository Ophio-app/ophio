// SPDX-License-Identifier: Apache-2.0
//
// Relative ages. One shared ticker drives every visible age so a screen full
// of timestamps re-renders once per tick rather than once per timestamp.

import { useSyncExternalStore } from "react";

const SEC = 1000;
const MIN = 60 * SEC;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

/** Short visual age: "now", "12 s", "4 min", "2 h", "3 d". */
export function shortAge(ms: number): string {
  if (ms < 5 * SEC) return "now";
  if (ms < MIN) return `${Math.floor(ms / SEC)} s`;
  if (ms < HOUR) return `${Math.floor(ms / MIN)} min`;
  if (ms < DAY) return `${Math.floor(ms / HOUR)} h`;
  return `${Math.floor(ms / DAY)} d`;
}

/** Spoken age for assistive technology: "12 seconds ago". */
export function spokenAge(ms: number): string {
  if (ms < 5 * SEC) return "just now";
  const unit = (n: number, w: string) => `${n} ${w}${n === 1 ? "" : "s"} ago`;
  if (ms < MIN) return unit(Math.floor(ms / SEC), "second");
  if (ms < HOUR) return unit(Math.floor(ms / MIN), "minute");
  if (ms < DAY) return unit(Math.floor(ms / HOUR), "hour");
  return unit(Math.floor(ms / DAY), "day");
}

/** Wall-clock time, e.g. "14:07", with the date when it is not today. */
export function clockTime(at: number, now = Date.now()): string {
  const d = new Date(at);
  const sameDay = new Date(now).toDateString() === d.toDateString();
  const time = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  return sameDay ? time : `${d.toLocaleDateString(undefined, { month: "short", day: "numeric" })}, ${time}`;
}

export function duration(ms: number): string {
  if (ms < MIN) return `${Math.max(1, Math.round(ms / SEC))} s`;
  if (ms < HOUR) return `${Math.floor(ms / MIN)} min ${Math.floor((ms % MIN) / SEC)} s`;
  return `${Math.floor(ms / HOUR)} h ${Math.floor((ms % HOUR) / MIN)} min`;
}

// ---- Shared ticker --------------------------------------------------------

let now = Date.now();
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setInterval> | null = null;

function tick(): void {
  now = Date.now();
  for (const x of listeners) x();
}

function start(): void {
  if (!timer && document.visibilityState !== "hidden") timer = setInterval(tick, SEC);
}

function stop(): void {
  if (timer) clearInterval(timer);
  timer = null;
}

/** Nothing on screen can be seen while the app is hidden, so the ticker
 * rests and wakes nobody; shown again, every watcher gets the time at once. */
function visibility(): void {
  if (document.visibilityState === "hidden") return stop();
  if (timer) return;
  tick();
  start();
}

function subscribe(l: () => void): () => void {
  if (listeners.size === 0) document.addEventListener("visibilitychange", visibility);
  listeners.add(l);
  start();
  return () => {
    listeners.delete(l);
    if (listeners.size > 0) return;
    stop();
    document.removeEventListener("visibilitychange", visibility);
  };
}

/** Current time, refreshed every second while any component is watching and the app is shown. */
export function useNow(): number {
  return useSyncExternalStore(subscribe, () => now, () => now);
}
