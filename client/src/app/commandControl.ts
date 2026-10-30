// SPDX-License-Identifier: Apache-2.0
//
// The interaction contract (docs/interaction-contract.md) for a control that
// changes a setting on the computer.
//
// An instant control shows the person's choice on the tap and sends it
// behind; it stays usable meanwhile, and taps made while one is on its way
// go once it is answered, as the latest choice. A refusal puts back what the
// computer has and says what didn't change. A safety control keeps showing
// what is in force, with the choice beside it as changing, until the
// computer says the choice is in force.

import { useEffect, useRef, useState } from "react";
import { announce } from "../design/Announcer";
import { toast } from "../design/Toast";
import { SETTLE_MS } from "../host/expected";

export type ControlKind = "instant" | "safety";

/** One change, in the person's words. */
export interface ChangeNote {
  /** For the computer's command history: "Turn off questions alerts". */
  label: string;
  /** What a refusal says it couldn't change: "questions alerts". */
  what: string;
  /** Said to a screen reader once the computer has it. */
  said?: string;
  /** Shown as a toast once the computer has it, for a change worth
   * confirming out loud ("GitHub is on"). */
  done?: string;
}

export interface ControlOptions<T> {
  kind: ControlKind;
  /** What the computer has. */
  value: T;
  /** Sends a change. Resolves once the computer has it (for a safety
   * control, once it is in force); rejects when it refuses. */
  send: (next: T, note: ChangeNote) => Promise<unknown>;
  /** Why a refusal happened, in the person's words. */
  describe: (error: unknown) => string;
  equal?: (a: T, b: T) => boolean;
}

export interface CommandControl<T> {
  /** What the control shows as chosen. */
  value: T;
  /** A safety control's choice still on its way, once it has taken long
   * enough to show as Changing…; null otherwise. */
  changing: T | null;
  /** The choice still on its way, once it has taken long enough to show as
   * changing, whatever the kind; null otherwise. A control holding several
   * choices, some of them safety ones, shows those as changing with it. */
  waiting: T | null;
  /** The choice on its way, from the tap, whatever the kind; null otherwise.
   * A safety control holding several choices in one record builds the next
   * tap on it, so a quick second tap doesn't undo the first. */
  pending: T | null;
  /** Makes a change. Resolves true once the computer has the person's
   * latest choice, false when it refuses. */
  set(next: T, note: ChangeNote): Promise<boolean>;
}

/** A change taking longer than this shows as changing, so a quick one
 * doesn't flicker. */
export const CHANGING_AFTER_MS = 250;

interface Wanted<T> { value: T; note: ChangeNote }

export function useCommandControl<T>(opts: ControlOptions<T>): CommandControl<T> {
  const equal = opts.equal ?? Object.is;
  const latest = useRef(opts);
  latest.current = opts;
  const [wanted, setWanted] = useState<Wanted<T> | null>(null);
  const [slow, setSlow] = useState(false);
  const live = useRef({
    wanted: null as Wanted<T> | null,
    sending: false,
    // The latest choice made while one was on its way.
    queued: null as Wanted<T> | null,
    // The computer has the latest choice; it goes once its state shows it.
    accepted: false,
    // Who asked how the latest choice went.
    told: [] as Array<(ok: boolean) => void>,
    timers: new Set<ReturnType<typeof setTimeout>>(),
    mounted: true,
  });

  const later = (ms: number, run: () => void) => {
    const l = live.current;
    const timer = setTimeout(() => { l.timers.delete(timer); run(); }, ms);
    l.timers.add(timer);
  };
  const clear = () => {
    const l = live.current;
    l.wanted = null; l.queued = null; l.accepted = false;
    for (const t of l.timers) clearTimeout(t);
    l.timers.clear();
    if (l.mounted) { setWanted(null); setSlow(false); }
  };
  const tell = (ok: boolean) => {
    const told = live.current.told;
    live.current.told = [];
    for (const t of told) t(ok);
  };
  const run = (w: Wanted<T>) => {
    const l = live.current;
    l.sending = true; l.accepted = false;
    void latest.current.send(w.value, w.note).then(() => {
      l.sending = false;
      if (l.queued) {
        const next = l.queued;
        l.queued = null;
        run(next);
        return;
      }
      if (w.note.said) announce(w.note.said);
      if (w.note.done) toast(w.note.done);
      tell(true);
      if (l.wanted !== w) return;
      l.accepted = true;
      if (equal(latest.current.value, w.value)) clear();
      // The computer's own state can lag its answer; stop waiting on it.
      else later(SETTLE_MS, () => { if (l.wanted === w) clear(); });
    }, (e: unknown) => {
      l.sending = false;
      clear();
      tell(false);
      toast(`Couldn’t change ${w.note.what}. ${latest.current.describe(e)}`, { tone: "stop" });
    });
  };
  const set = (value: T, note: ChangeNote) => {
    const l = live.current;
    if (equal(value, l.wanted?.value ?? latest.current.value)) {
      return l.sending ? new Promise<boolean>((r) => l.told.push(r)) : Promise.resolve(true);
    }
    const told = new Promise<boolean>((r) => l.told.push(r));
    const w = { value, note };
    l.wanted = w;
    setWanted(w);
    later(CHANGING_AFTER_MS, () => { if (l.wanted === w && l.mounted) setSlow(true); });
    if (l.sending) l.queued = w;
    else run(w);
    return told;
  };

  // Taken off once the computer's state shows the choice.
  useEffect(() => {
    const l = live.current;
    if (wanted && l.accepted && !l.sending && l.wanted === wanted && equal(opts.value, wanted.value)) clear();
  });
  useEffect(() => {
    const l = live.current;
    l.mounted = true;
    return () => { l.mounted = false; for (const t of l.timers) clearTimeout(t); l.timers.clear(); };
  }, []);

  const instant = opts.kind === "instant";
  const waiting = wanted && slow ? wanted.value : null;
  return {
    value: instant && wanted ? wanted.value : opts.value,
    changing: instant ? null : waiting,
    waiting,
    pending: wanted ? wanted.value : null,
    set,
  };
}
