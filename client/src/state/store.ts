// SPDX-License-Identifier: Apache-2.0
//
// A minimal external store. State is replaced immutably; components select
// the slice they render so unrelated updates do not re-render them.

import { useCallback, useRef, useSyncExternalStore } from "react";

type Listener = () => void;

export class Store<T> {
  #state: T;
  #listeners = new Set<Listener>();

  constructor(initial: T) {
    this.#state = initial;
  }

  get = (): T => this.#state;

  set(next: T | ((prev: T) => T)): void {
    const value = typeof next === "function" ? (next as (prev: T) => T)(this.#state) : next;
    if (Object.is(value, this.#state)) return;
    this.#state = value;
    for (const l of this.#listeners) l();
  }

  subscribe = (listener: Listener): (() => void) => {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  };
}

export function shallowEqual<T>(a: T, b: T): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  for (const k of ka) {
    if (!Object.is((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k])) return false;
  }
  return true;
}

/** Subscribe to a derived slice of a store. */
export function useStore<T, S>(
  store: Store<T>,
  selector: (state: T) => S,
  equal: (a: S, b: S) => boolean = Object.is,
): S {
  const memo = useRef<{ state: T; selector: (state: T) => S; value: S } | null>(null);
  const select = useCallback(() => {
    const state = store.get();
    const prev = memo.current;
    if (prev && Object.is(prev.state, state) && prev.selector === selector) return prev.value;
    const value = selector(state);
    if (prev && equal(prev.value, value)) {
      memo.current = { state, selector, value: prev.value };
      return prev.value;
    }
    memo.current = { state, selector, value };
    return value;
  }, [store, selector, equal]);
  return useSyncExternalStore(store.subscribe, select, select);
}
