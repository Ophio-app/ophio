// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from "react";

export function readLocal<T>(key: string, fallback: T): T {
  try {
    return (
      (JSON.parse(localStorage.getItem(`work.${key}`) ?? "null") as T) ??
      fallback
    );
  } catch {
    return fallback;
  }
}
export function writeLocal(key: string, value: unknown): boolean {
  try {
    localStorage.setItem(`work.${key}`, JSON.stringify(value));
    return true;
  } catch {
    return false;
  }
}
const textListeners = new Set<(key: string, value: string, saved: boolean) => void>();

/** Persist immediately and share edits with other mounted forms for this key.
 * A storage event alone would not notify forms in the same window. */
export function useLocalText(key: string, initial = "") {
  const [value, setValue] = useState(() => {
    const saved = readLocal<unknown>(key, initial);
    return typeof saved === "string" ? saved : initial;
  });
  const [saved, setSaved] = useState(true);
  useEffect(() => {
    const changed = (changedKey: string, next: string, stored: boolean) => {
      if (changedKey !== key) return;
      setValue(next);
      setSaved(stored);
    };
    textListeners.add(changed);
    return () => { textListeners.delete(changed); };
  }, [key]);
  return [
    value,
    (next: string) => {
      const stored = writeLocal(key, next);
      setValue(next);
      setSaved(stored);
      for (const changed of textListeners) changed(key, next, stored);
    },
    saved,
  ] as const;
}

/** Freeze a visit’s cutoff; save only the last event observed when leaving.
 * `heardAt` is read then, so the screen need not render each time it moves. */
export function useVisitCutoff(
  key: string,
  active: boolean,
  heardAt: () => number | null,
) {
  const read = () => {
    const value = readLocal<unknown>(key, 0);
    return typeof value === "number" && Number.isFinite(value) ? value : 0;
  };
  const [since, setSince] = useState(read);
  const wasActive = useRef(false);
  const heard = useRef(heardAt);
  heard.current = heardAt;
  useEffect(() => {
    if (!active) {
      wasActive.current = false;
      return;
    }
    // StrictMode may repeat setup/cleanup without starting a new visit.
    if (!wasActive.current) setSince(read());
    wasActive.current = true;
    const save = () => {
      const at = heard.current();
      if (at !== null) writeLocal(key, at);
    };
    window.addEventListener("pagehide", save);
    return () => {
      window.removeEventListener("pagehide", save);
      save();
    };
  }, [active, key]);
  return since;
}
