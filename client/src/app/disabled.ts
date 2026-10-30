// SPDX-License-Identifier: Apache-2.0
//
// Disable in Øphio (D-071): the skills, agents and agent files the person
// keeps from every agent Øphio runs. The computer keeps the list; the files
// themselves never change. A tap shows its result at once and goes back,
// with the computer's reason, if the computer refuses it.

import { useCallback, useEffect, useMemo, useSyncExternalStore } from "react";
import { describeError, useApp, useHost, useLink } from "./context";
import { NotConnectedError, OutcomeUnknownError } from "../host/session";
import { toast } from "../design/Toast";
import { useMay } from "../shell/NotAllowed";
import { useOnScreen } from "../shell/Screen";
import type { AgentFileGroup, CatalogFile, Disable, DisabledItem, DisabledKind, HarnessKind } from "../protocol/types";

/** What a computer that can keep things from its agents advertises. */
export const DISABLE_FEATURE = "disable_in_ophio_v1";

/** How long a change the computer accepted waits for its list to show it. */
const SETTLE_MS = 5_000;

/** What disabling one item sends, and what is shown for it until the
 * computer's list carries it. */
export interface DisableTarget {
  kind: DisabledKind;
  host_path: string;
  /** Computer list identity, which may differ from the listed link path. */
  key?: string;
  name: string;
  description?: string | null;
  scope: string;
  harness?: HarnessKind | null;
  project_id?: string | null;
  group?: AgentFileGroup | null;
  library?: CatalogFile | null;
}

interface State {
  /** The computer's list as last read; null before the first read. */
  items: DisabledItem[] | null;
  loading: boolean;
  error: string | null;
  /** What this device asked for and the computer's list doesn't show yet. */
  pending: Map<string, { disabled: boolean; item: DisabledItem; unknown?: boolean; readAfter: number; answered?: boolean }>;
}

class Store {
  state: State = { items: null, loading: false, error: null, pending: new Map() };
  /** The read in flight, which every screen asking now shares. */
  reading: Promise<void> | null = null;
  /** Incremented when a list read starts, not when it finishes. */
  readSequence = 0;
  #listeners = new Set<() => void>();
  subscribe = (listener: () => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };
  get = () => this.state;
  set(change: Partial<State>) {
    this.state = { ...this.state, ...change };
    for (const l of this.#listeners) l();
  }
  /** Takes back only changes confirmed by a read begun after their answer. */
  settle(readSequence: number) {
    const items = this.state.items;
    if (!items || !this.state.pending.size) return;
    const listed = new Set(items.map((i) => i.host_path));
    const pending = new Map([...this.state.pending].filter(([path, p]) =>
      !p.answered || readSequence <= p.readAfter || (!p.unknown && listed.has(path) !== p.disabled)));
    if (pending.size !== this.state.pending.size) this.set({ pending });
  }
  drop(path: string) {
    if (!this.state.pending.has(path)) return;
    const pending = new Map(this.state.pending);
    pending.delete(path);
    this.set({ pending });
  }
}

/** One list for each computer, shared by every screen showing it. */
const stores = new Map<string, Store>();
function storeFor(profileId: string): Store {
  let s = stores.get(profileId);
  if (!s) stores.set(profileId, (s = new Store()));
  return s;
}

/** For tests: forget every computer's list. */
export function resetDisabled() {
  stores.clear();
}

/** Whether a row from a list the computer gave is disabled, as the
 * computer said it. */
export function listedDisabled(disable: Disable | undefined): boolean {
  return disable?.state === "can" && disable.disabled;
}

export interface Disabled {
  /** The computer can keep things from its agents. */
  supported: boolean;
  /** This device may read the list, and change it. */
  allowed: boolean;
  canChange: boolean;
  /** Everything disabled, newest first, with this device's taps shown. */
  items: DisabledItem[];
  /** Read at least once. */
  read: boolean;
  loading: boolean;
  error: string | null;
  refresh: () => void;
  /** Whether the computer-list `key` is disabled. Without a key, trust `listed`. */
  isDisabled: (key: string, listed?: boolean, hasKey?: boolean) => boolean;
  /** Disable or enable one item: shown at once, put back if refused. */
  set: (target: DisableTarget, disabled: boolean) => void;
}

export function useDisabled(): Disabled {
  const { session, profile } = useApp();
  const online = useLink().state === "online";
  const supported = useHost((s) => !!s.capabilities?.features.includes(DISABLE_FEATURE));
  const allowed = useMay("library_read");
  const canChange = useHost((s) => !!s.grants?.library_write);
  const store = storeFor(profile.profileId);
  const state = useSyncExternalStore(store.subscribe, store.get);

  const refresh = useCallback(() => {
    if (!supported || !allowed || !online || store.reading) return;
    store.set({ loading: true, error: null });
    const readSequence = ++store.readSequence;
    store.reading = session.query({ kind: "disabled_in_ophio" }).then(
      (r) => {
        store.set({ items: r.items, loading: false, error: null });
        store.settle(readSequence);
      },
      (e: unknown) => store.set({ loading: false, error: describeError(e, profile.name) }),
    ).finally(() => { store.reading = null; });
  }, [session, store, supported, allowed, online, profile.name]);

  // Read again each time a screen showing it comes back into view, as
  // another device may have changed it.
  const active = useOnScreen();
  useEffect(() => { if (active) refresh(); }, [refresh, active]);

  const items = useMemo(() => {
    const base = state.items ?? [];
    const shown = base.filter((i) => state.pending.get(i.host_path)?.disabled !== false);
    for (const [path, p] of state.pending) if (p.disabled && !base.some((i) => i.host_path === path)) shown.push(p.item);
    return shown.sort((a, b) => b.disabled_at - a.disabled_at);
  }, [state.items, state.pending]);

  const isDisabled = useCallback((key: string, listed?: boolean, hasKey = true) => {
    if (!supported) return false;
    const asked = state.pending.get(key);
    if (asked) return asked.disabled;
    if (hasKey && state.items) return state.items.some((i) => i.host_path === key);
    return !!listed;
  }, [state.items, state.pending, supported]);

  const set = useCallback((target: DisableTarget, disabled: boolean) => {
    const key = target.key ?? target.host_path;
    const item: DisabledItem = {
      kind: target.kind,
      host_path: key,
      name: target.name,
      description: target.description ?? null,
      scope: target.scope,
      harness: target.harness ?? null,
      project_id: target.project_id ?? null,
      group: target.group ?? null,
      library: target.library ?? null,
      disabled_at: state.items?.find((i) => i.host_path === key)?.disabled_at ?? Date.now(),
      found: true,
    };
    store.set({ pending: new Map(store.get().pending).set(key, { disabled, item, readAfter: store.readSequence }) });
    void session
      .command(
        { command: "set_disabled_in_ophio", kind: target.kind, host_path: target.host_path, disabled },
        { label: `${disabled ? "Disable" : "Enable"} ${target.name} in Øphio`, expectedRevision: null },
      )
      .then(
        () => {
          const pending = new Map(store.get().pending);
          const asked = pending.get(key);
          if (asked) pending.set(key, { ...asked, answered: true, readAfter: store.readSequence });
          store.set({ pending });
          // A read already in flight may predate this change.
          void (store.reading ?? Promise.resolve()).then(refresh);
          setTimeout(() => { if (!store.get().pending.get(key)?.unknown) store.drop(key); }, SETTLE_MS);
        },
        (e: unknown) => {
          if (e instanceof OutcomeUnknownError || e instanceof NotConnectedError) {
            const pending = new Map(store.get().pending);
            const asked = pending.get(key);
            if (asked) pending.set(key, { ...asked, unknown: true, answered: true, readAfter: store.readSequence });
            store.set({ pending });
            void (store.reading ?? Promise.resolve()).then(refresh);
            toast(`Not sure ${profile.name} got that. Checking again when it’s back. ${describeError(e, profile.name)}`);
          } else {
            store.drop(key);
            toast(`${target.name} is still ${disabled ? "enabled" : "disabled"}. ${describeError(e, profile.name)}`, { tone: "stop" });
            void (store.reading ?? Promise.resolve()).then(refresh);
          }
        },
      );
  }, [session, store, refresh, profile.name, state.items]);

  return {
    supported,
    allowed,
    canChange,
    items: supported ? items : [],
    read: state.items !== null,
    loading: state.loading,
    error: state.error,
    refresh,
    isDisabled,
    set,
  };
}

/** "Skill", "Agent", or what kind of file. */
export function kindLabel(item: { kind: DisabledKind; group?: AgentFileGroup | null }): string {
  switch (item.group) {
    case "skills": return "Skill";
    case "agents": return "Agent";
    case "instructions": return "Instructions";
    case "commands": return "Command";
    case "rules": return "Rules";
    case "memory": return "Memory";
    default: return item.kind === "skill" ? "Skill" : item.kind === "agent" ? "Agent" : "File";
  }
}

/** What disabling does, said once wherever the choice is offered. */
export const DISABLE_ABOUT =
  "Agents that Øphio runs won’t see it, list it or use it. The file isn’t changed, and Claude Code and Codex used outside Øphio still have it.";
