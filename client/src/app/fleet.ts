// SPDX-License-Identifier: Apache-2.0
//
// Reading every paired computer at once: for the header's menu, which shows
// how each is doing, and for All, which lists what all of them hold.

import { useCallback, useMemo, useRef, useSyncExternalStore } from "react";
import type { HostProfile, LinkState } from "../host/link";
import type { HostSession } from "../host/session";
import { freshness, type Freshness } from "../shell/freshness";
import { emptyHostState, openAttention, reviewAttention, type HostState } from "../state/projection";
import type { AttentionItem } from "../protocol/types";
import { Store, useStore } from "../state/store";
import { useApp } from "./context";

export interface ComputerEntry {
  profile: HostProfile;
  /** Absent while the app, out of sight, has let go of its connection. */
  session: HostSession | undefined;
  current: boolean;
}

const NO_SESSIONS = new Map<string, HostSession>();
const noStore = { get: () => NO_SESSIONS, subscribe: () => () => {} };

/** Every paired computer, the one shown first among equals in pairing order. */
export function useComputerEntries(): ComputerEntry[] {
  const { computers, profiles, profile, session } = useApp();
  const sessions = useSyncExternalStore(computers?.subscribe ?? noStore.subscribe, computers?.get ?? noStore.get);
  return useMemo(() => {
    const list = profiles.some((p) => p.profileId === profile.profileId) ? profiles : [profile, ...profiles];
    return list.map((p) => {
      const current = p.profileId === profile.profileId;
      return { profile: p, session: current ? session : sessions.get(p.profileId), current };
    });
  }, [profiles, profile, session, sessions]);
}

/** What All lists of a computer under Needs you, or with `review` under To
 * look at: each item of a task in a project it knows. The tab bar counts
 * the same. */
export function listedEverywhere(s: HostState, review = false): AttentionItem[] {
  return (review ? reviewAttention(s) : openAttention(s)).filter((a) => {
    const task = a.task_id ? s.tasks[a.task_id] : undefined;
    return !!task && !!s.projects[task.project_id];
  });
}

/** Out of reach, as the header counts it: one trying again after it went
 * offline still is, so nothing flickers with each attempt. */
export function outOfReach(link: LinkState): boolean {
  return link.state === "offline" || link.state === "failed" || (link.state === "connecting" && link.attempt > 1);
}

const idle: LinkState = { state: "idle" };
const ALWAYS = () => () => {};
/** How current one computer's picture is, from its session's link alone,
 * as the header's pill reads it. */
export function useComputerFreshness(entry: ComputerEntry): Freshness {
  const session = entry.session;
  const link = useStore(session?.link ?? IDLE_LINK, (l) => l);
  const caughtUp = useStore(session?.state ?? NO_STATE, (s) => s.caughtUp);
  const unanswered = useStore(session?.unanswered ?? NOT_ASKED, (u) => u);
  return freshness(session ? link : idle, caughtUp, null, 0, entry.profile.name, unanswered);
}

/** One value derived from every computer's state and link, read again only
 * when one of them changes and kept while `equal` says it is the same.
 * `uses` names the parts `select` reads, where that is less than the whole
 * state, so words streaming in don't make it read again. */
export function useEveryComputer<T>(entries: ComputerEntry[], select: (entry: ComputerEntry, state: HostState | null, link: LinkState) => T, equal: (a: T[], b: T[]) => boolean,
  uses: (state: HostState | undefined) => unknown = (s) => s): T[] {
  const memo = useRef<{ inputs: unknown[]; value: T[] } | null>(null);
  const subscribe = useCallback((listener: () => void) => {
    const offs = entries.flatMap((e) => e.session ? [e.session.state.subscribe(listener), e.session.link.subscribe(listener)] : []);
    return offs.length ? () => { for (const off of offs) off(); } : ALWAYS();
  }, [entries]);
  const read = useCallback(() => {
    // `uses` may name several parts, each compared on its own.
    const inputs = entries.flatMap((e) => { const used = uses(e.session?.state.get()); return [e, ...(Array.isArray(used) ? used : [used]), e.session?.link.get()]; });
    const last = memo.current;
    if (last && last.inputs.length === inputs.length && last.inputs.every((x, i) => x === inputs[i])) return last.value;
    const value = entries.map((e) => select(e, e.session?.state.get() ?? null, e.session?.link.get() ?? idle));
    memo.current = { inputs, value: last && equal(last.value, value) ? last.value : value };
    return memo.current.value;
  }, [entries, select, equal, uses]);
  return useSyncExternalStore(subscribe, read, read);
}

const IDLE_LINK = new Store<LinkState>(idle);
const NO_STATE = new Store<HostState>(emptyHostState);
const NOT_ASKED = new Store(false);
