// SPDX-License-Identifier: Apache-2.0
export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** The damaged value is still the only copy; callers must not save over it. */
export class UnpreservedJson extends Error {
  constructor(cause: unknown) {
    super("Damaged saved data could not be set aside", { cause });
  }
}

/** Keep every distinct recovery value; retrying the same copy is harmless. */
export function preserveSavedText(storage: StorageLike, key: string, text: string): void {
  for (let n = 0; ; n++) {
    const backup = `${key}${n === 0 ? "" : `.${n}`}`;
    const existing = storage.getItem(backup);
    if (existing === text) return;
    if (existing === null) {
      storage.setItem(backup, text);
      return;
    }
  }
}

/** Only a parse failure permits starting fresh, and only after the exact value
 * has a separate durable copy. Read failures still leave the contents unknown. */
export function readSavedJson(storage: StorageLike, key: string): {
  value: unknown;
  recovered: boolean;
} {
  const text = storage.getItem(key);
  if (text === null) return { value: null, recovered: false };
  try {
    return { value: JSON.parse(text), recovered: false };
  } catch (error) {
    if (!(error instanceof SyntaxError)) throw error;
  }
  try {
    // Reuse a copy of the same value after a restart before the first save.
    // Later corruption must not erase an earlier recovery copy.
    preserveSavedText(storage, `${key}.unreadable`, text);
  } catch (error) {
    throw new UnpreservedJson(error);
  }
  return { value: null, recovered: true };
}
