// SPDX-License-Identifier: Apache-2.0
//
// Short confirmation of a local action's outcome, shown above the screen's
// pinned footer, the safety rail and navigation. Never the only record of a
// result.
//
// The text is announced once through the live regions; the toast itself sits
// in a labelled region so its action and Dismiss stay reachable by assistive
// technology without being read out a second time. While focus is inside a
// toast it stays up, so reaching its action never races the timer.

import { CircleAlert, CircleCheck, Info, X } from "lucide-react";
import { useSyncExternalStore } from "react";
import { openReport } from "../reports/request";
import { recordNotice } from "../diagnostics/instrumentation";
import { announce, withdraw } from "./Announcer";

interface ToastItem {
  id: number;
  text: string;
  /** `good` is a setting the person just turned on or off, in green. */
  tone: "info" | "stop" | "good";
  action?: { label: string; run: () => void };
  /** Runs when the toast goes without its action taken: an undo window closes. */
  expire?: () => void;
}

let items: ToastItem[] = [];
let seq = 0;
const listeners = new Set<() => void>();
const timers = new Map<number, ReturnType<typeof setTimeout>>();

function emit() {
  for (const l of listeners) l();
}

function dismiss(id: number, acted = false) {
  const item = items.find((t) => t.id === id);
  clearTimeout(timers.get(id));
  timers.delete(id);
  items = items.filter((t) => t.id !== id);
  emit();
  // Off the screen, its words are no longer the current announcement either.
  if (item) withdraw(item.text);
  if (!acted) item?.expire?.();
}

function schedule(item: ToastItem) {
  clearTimeout(timers.get(item.id));
  timers.set(item.id, setTimeout(() => dismiss(item.id), item.action ? 7000 : item.tone === "stop" ? 6000 : 3500));
}

export function toast(text: string, opts: { code?: string; tone?: ToastItem["tone"]; action?: ToastItem["action"]; expire?: ToastItem["expire"] } = {}): void {
  recordNotice(opts.code ?? "toast", opts.tone === "stop" ? "error" : "info");
  const item: ToastItem = { id: ++seq, text, tone: opts.tone ?? "info", action: opts.action ?? (opts.tone === "stop" ? { label: "Report", run: () => openReport({ type: "error" }, { error: { code: opts.code ?? "app_error", message: text } }) } : undefined), expire: opts.expire };
  for (const old of items.slice(0, -1)) dismiss(old.id);
  items = [...items, item];
  emit();
  announce(text, item.tone === "stop" ? "assertive" : "polite");
  schedule(item);
}

export function Toasts() {
  const list = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => items,
  );
  if (list.length === 0) return null;
  return (
    <section className="toasts" aria-label="Notifications">
      {list.map((t) => (
        <div
          key={t.id}
          className={`toast toast--${t.tone}`}
          onFocus={() => clearTimeout(timers.get(t.id))}
          onBlur={(e) => {
            if (!e.currentTarget.contains(e.relatedTarget as Node | null)) schedule(t);
          }}
        >
          {t.tone === "stop" ? <CircleAlert className="toast__icon" /> : t.tone === "good" ? <CircleCheck className="toast__icon" /> : <Info className="toast__icon" />}
          <p className="toast__text">{t.text}</p>
          {t.action && (
            <button
              type="button"
              className="toast__action"
              onClick={() => {
                t.action?.run();
                dismiss(t.id, true);
              }}
            >
              {t.action.label}
            </button>
          )}
          <button type="button" className="toast__close" aria-label="Dismiss" title="Dismiss" onClick={() => dismiss(t.id)}>
            <X />
          </button>
        </div>
      ))}
    </section>
  );
}
