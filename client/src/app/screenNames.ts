// SPDX-License-Identifier: Apache-2.0
import type { ControlTarget } from "../protocol/types";
import type { HostState } from "../state/projection";
import { Store } from "../state/store";
import { prefs } from "./prefs";

export const SCREEN_NAME_MAX = 40;

const key = (profileId: string, t: ControlTarget) => `${profileId} ${t.native_ref}`;

/** The connector a screen is plugged into, as the computer names it. Windows
 * prefixes its display names with `\\.\`. */
export function connector(t: ControlTarget): string {
  return t.native_ref.replace(/^\\\\\.\\/, "");
}

/** A screen's number in the computer's own order, with its connector so it
 * can be found at the computer: "Screen 1 (DP-1)". */
export function numberedName(targets: ControlTarget[], t: ControlTarget): string {
  const n = targets.filter((x) => x.kind === "output").findIndex((x) => x.target_id === t.target_id) + 1;
  return `Screen ${n || 1} (${connector(t)})`;
}

/** Labels screens with their numbered name, or a name given on this device.
 * Windows and the whole desktop keep the computer's label. Built from the
 * connector, not the label, so naming named targets changes nothing. */
export function nameScreens(targets: ControlTarget[], named: (t: ControlTarget) => string | undefined): ControlTarget[] {
  let changed = false;
  const out = targets.map((t) => {
    if (t.kind !== "output") return t;
    const label = named(t) || numberedName(targets, t);
    if (label === t.label) return t;
    changed = true;
    return { ...t, label };
  });
  return changed ? out : targets;
}

export function screenName(profileId: string, t: ControlTarget): string | undefined {
  return prefs.get().screenNames?.[key(profileId, t)];
}

/** Saves a screen's name on this device; an empty name restores the numbered one. */
export function renameScreen(profileId: string, t: ControlTarget, name: string): void {
  const k = key(profileId, t);
  const trimmed = name.trim().slice(0, SCREEN_NAME_MAX);
  prefs.set((p) => {
    const screenNames = { ...p.screenNames };
    if (trimmed) screenNames[k] = trimmed;
    else delete screenNames[k];
    return { ...p, screenNames };
  });
}

/** A session's state, with its screens named as above whenever the computer
 * reports them, and what this device has just asked for shown over it until
 * the computer's own state catches up (see `expect`). */
export class HostStateStore extends Store<HostState> {
  readonly #profileId: () => string;
  #base: HostState;
  readonly #expected = new Map<number, (s: HostState) => HostState>();
  #nextExpected = 0;

  constructor(initial: HostState, profileId: () => string) {
    super(initial);
    this.#base = initial;
    this.#profileId = profileId;
  }

  #name(targets: ControlTarget[]): ControlTarget[] {
    const profileId = this.#profileId();
    return nameScreens(targets, (t) => screenName(profileId, t));
  }

  /** The computer's state as it last said, without what is expected of it. */
  base = (): HostState => this.#base;

  override set(next: HostState | ((prev: HostState) => HostState)): void {
    const prev = this.#base;
    const value = typeof next === "function" ? next(prev) : next;
    if (Object.is(value, prev)) return;
    this.#base = value.targets === prev.targets ? value : { ...value, targets: this.#name(value.targets) };
    this.#show();
  }

  #show(): void {
    let shown = this.#base;
    for (const change of this.#expected.values()) shown = change(shown);
    super.set(shown);
  }

  /** Shows a name just given on this device. */
  renamed(): void {
    const targets = this.#name(this.#base.targets);
    if (targets === this.#base.targets) return;
    this.#base = { ...this.#base, targets };
    this.#show();
  }

  /**
   * Shows `change` at once, as though the computer had already made it, until
   * the returned function takes it off. `change` returns its argument where
   * there is nothing left to change, so the computer's own word shows as is.
   */
  expect(change: (s: HostState) => HostState): () => void {
    const id = this.#nextExpected++;
    this.#expected.set(id, change);
    this.#show();
    return () => { if (this.#expected.delete(id)) this.#show(); };
  }
}
