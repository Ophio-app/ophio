// SPDX-License-Identifier: Apache-2.0
import { readSavedJson, type StorageLike } from "../state/savedJson";

export function localStorageOrNull(): StorageLike | null {
  try { return globalThis.localStorage ?? null; } catch { return null; }
}

export class SavedDiagnostics<T> {
  #writable = true;
  constructor(readonly key: string, readonly storage = localStorageOrNull()) {}
  load(fallback: T): T {
    try { return (this.storage ? readSavedJson(this.storage, this.key).value as T | null : null) ?? fallback; }
    catch { this.#writable = false; return fallback; }
  }
  save(value: T): boolean {
    if (!this.storage || !this.#writable) return false;
    try { this.storage.setItem(this.key, JSON.stringify(value)); return true; } catch { return false; }
  }
}
