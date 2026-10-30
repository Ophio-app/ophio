// SPDX-License-Identifier: Apache-2.0
//
// Screen-reader announcements for meaningful state changes (a command's
// outcome, an ownership change). Streamed text and tool events are not
// announced one by one.

import { useSyncExternalStore } from "react";

interface Msg {
  text: string;
  urgency: "polite" | "assertive";
  n: number;
}

// One current message: a new one replaces the last, in either region, so a
// screen reader never finds an older message beside it.
let current: Msg = { text: "", urgency: "polite", n: 0 };
const listeners = new Set<() => void>();

// Long enough for any screen reader to have taken the words when they arrived.
const READ_MS = 10_000;
let expiry: ReturnType<typeof setTimeout> | undefined;

export function announce(text: string, urgency: "polite" | "assertive" = "polite"): void {
  const n = current.n + 1;
  current = { text, urgency, n };
  for (const l of listeners) l();
  // A message left in place is a stop with nothing on screen for a screen
  // reader walking the page, so it goes once it has been read.
  clearTimeout(expiry);
  if (text) expiry = setTimeout(() => current.n === n && withdraw(text), READ_MS);
}

/** Clear a message once what it said is resolved, unless a newer one replaced it. */
export function withdraw(text: string): void {
  if (current.text !== text) return;
  current = { ...current, text: "" };
  for (const l of listeners) l();
}

function subscribe(l: () => void) {
  listeners.add(l);
  return () => listeners.delete(l);
}

export function LiveRegions() {
  const m = useSyncExternalStore(subscribe, () => current);
  // The key forces a fresh node so repeating the same text is announced again.
  return (
    <div className="visually-hidden">
      <div aria-live="polite" aria-atomic="true">
        {m.urgency === "polite" && <span key={m.n}>{m.text}</span>}
      </div>
      <div aria-live="assertive" aria-atomic="true">
        {m.urgency === "assertive" && <span key={m.n}>{m.text}</span>}
      </div>
    </div>
  );
}
