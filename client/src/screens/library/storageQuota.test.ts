// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";
import { fileKey, LibraryStore, type StorageLike } from "./drafts";
import { READ_LIMIT } from "./model";
import type { FileContent } from "../../protocol/types";

/** A WebView's localStorage: about five million characters per origin. */
function quotaStorage(limit = 5 * 1024 * 1024): StorageLike & { data: Map<string, string>; attempts: number[] } {
  const data = new Map<string, string>();
  const attempts: number[] = [];
  return {
    data,
    attempts,
    getItem: (key) => data.get(key) ?? null,
    setItem: (key, value) => {
      attempts.push(value.length);
      let used = key.length + value.length;
      for (const [k, v] of data) if (k !== key) used += k.length + v.length;
      if (used > limit) throw new DOMException("The quota has been exceeded.", "QuotaExceededError");
      data.set(key, value);
    },
  };
}
const file = (path: string, text: string): FileContent => ({
  root_id: "notes", path, text, revision: `rev-${path}`, encoding: "utf8",
  newline: "lf", has_bom: false, modified_at: 1, size: text.length,
});

describe("Library storage under the device quota", () => {
  it("preserves unread drafts and the warning after an automatic roots refresh", () => {
    const storage = quotaStorage();
    const original = new LibraryStore("read-failure", storage);
    const note = file("todo.md", "before");
    original.saveDraft({ base: note, text: "unsent edit", updatedAt: 1, phase: "draft" });
    const saved = storage.data.get(original.key);
    const getItem = storage.getItem;
    storage.getItem = () => { throw new Error("Storage temporarily unavailable"); };
    const store = new LibraryStore("read-failure", storage);
    storage.getItem = getItem;
    storage.attempts.length = 0;
    const warning = store.get().storageError;
    expect(warning).toContain("could not be read");

    store.cacheRoots([]);

    expect.soft(storage.attempts).toHaveLength(0);
    expect.soft(storage.data.get(store.key)).toBe(saved);
    expect.soft(store.get().storageError).toBe(warning);
    expect(store.get().roots?.value).toEqual([]);
    const reopened = new LibraryStore("read-failure", storage);
    expect(reopened.draft("notes", "todo.md")?.text).toBe("unsent edit");
    expect(reopened.get().storageError).toBeNull();
    reopened.saveDraft({ base: note, text: "recovered edit", updatedAt: 2, phase: "draft" });
    expect(new LibraryStore("read-failure", storage).draft("notes", "todo.md")?.text).toBe("recovered edit");
  });

  it("keeps new changes in memory without replacing data that failed to load", () => {
    const storage = quotaStorage();
    const original = new LibraryStore("read-failure", storage);
    const note = file("todo.md", "before");
    original.saveDraft({ base: note, text: "saved edit", updatedAt: 1, phase: "draft" });
    const saved = storage.data.get(original.key);
    const getItem = storage.getItem;
    storage.getItem = () => { throw new Error("Storage temporarily unavailable"); };
    const store = new LibraryStore("read-failure", storage);
    storage.getItem = getItem;
    storage.attempts.length = 0;
    const warning = store.get().storageError;

    store.saveDraft({ base: note, text: "new edit", updatedAt: 2, phase: "draft" });
    store.form("name", "new form");
    store.cacheFile(note);
    store.cacheListing({ root_id: "notes", dir: "", entries: [] });
    store.cacheRoots([]);

    expect(storage.attempts).toHaveLength(0);
    expect(storage.data.get(store.key)).toBe(saved);
    expect(store.draft("notes", "todo.md")?.text).toBe("new edit");
    expect(store.get().forms.name).toBe("new form");
    expect(store.get().files[fileKey("notes", "todo.md")]?.value).toEqual(note);
    expect(store.get().listings[fileKey("notes", "")]?.value.entries).toEqual([]);
    expect(store.get().storageError).toBe(warning);
    expect(warning).toContain("held only until the app closes");
    expect(new LibraryStore("read-failure", storage).draft("notes", "todo.md")?.text).toBe("saved edit");
  });

  it("sets malformed data aside before saving drafts and forms that survive restart", () => {
    const storage = quotaStorage();
    const key = "library.v1.malformed";
    const saved = '{"version":1,"state":{"drafts":';
    storage.data.set(key, saved);
    const store = new LibraryStore("malformed", storage);
    expect(store.get().storageError).toBeNull();
    expect(store.get().storageRecovery).toContain("was damaged");
    expect(storage.data.get(`${key}.unreadable`)).toBe(saved);

    store.cacheRoots([]);
    store.form("name", "new form");
    store.saveDraft({ base: file("todo.md", "before"), text: "new edit", updatedAt: 2, phase: "draft" });

    expect(storage.data.get(`${key}.unreadable`)).toBe(saved);
    const reopened = new LibraryStore("malformed", storage);
    expect(reopened.draft("notes", "todo.md")?.text).toBe("new edit");
    expect(reopened.get().forms.name).toBe("new form");
    expect(reopened.get().storageError).toBeNull();
    expect(reopened.get().storageRecovery).toBeNull();
  });

  it("protects damaged data when its backup cannot fit, then recovers on restart", () => {
    const storage = quotaStorage(1);
    const key = "library.v1.full", saved = "{not json";
    storage.data.set(key, saved);
    const store = new LibraryStore("full", storage);
    expect(store.get().storageError).toContain("could not be set aside");
    expect(store.get().storageRecovery).toBeNull();
    storage.attempts.length = 0;
    store.form("name", "new form");
    expect(store.get().forms.name).toBe("new form");
    expect(storage.attempts).toHaveLength(0);
    expect([...storage.data.entries()]).toEqual([[key, saved]]);

    // Storage becomes writable again, then the app is reopened.
    storage.setItem = (key, value) => { storage.data.set(key, value); };
    const reopened = new LibraryStore("full", storage);
    reopened.form("name", "saved form");
    expect(new LibraryStore("full", storage).get().forms.name).toBe("saved form");
    expect(storage.data.get(`${key}.unreadable`)).toBe(saved);
  });

  it("can retry a failed draft write after the recovery copy succeeds", () => {
    const storage = quotaStorage();
    const key = "library.v1.retry", saved = "{broken";
    storage.data.set(key, saved);
    const store = new LibraryStore("retry", storage);
    const write = storage.setItem;
    storage.setItem = () => { throw new Error("Quota exceeded"); };
    store.form("name", "new form");
    expect(store.get().storageError).toContain("full");
    expect(storage.data.get(key)).toBe(saved);
    expect(storage.data.get(`${key}.unreadable`)).toBe(saved);
    storage.setItem = write;
    store.cacheRoots([]);
    expect(store.get().storageError).toBeNull();
    expect(new LibraryStore("retry", storage).get().forms.name).toBe("new form");
    expect(storage.data.get(`${key}.unreadable`)).toBe(saved);
  });

  it("evicts persisted caches while retaining live reads so subsequent edits each make one small write", () => {
    const storage = quotaStorage();
    const store = new LibraryStore("quota", storage);
    const note = file("todo.md", "- milk\n");
    store.saveDraft({ base: note, text: "- milk\n- eggs\n", updatedAt: 1, phase: "draft" });
    store.saveDraft({ base: file("pending.md", "original"), text: "pending", updatedAt: 2,
      phase: "unknown", recoveryId: "pending-recovery" });
    store.saveDraft({ base: file("discarded.md", "original"), text: "discarded", updatedAt: 3,
      phase: "unknown", recoveryId: "discarded-recovery" });
    store.discardDraft("notes", "discarded.md");
    const drafts = store.get().drafts;
    const uncheckedSaves = store.get().uncheckedSaves;
    store.cacheListing({ root_id: "notes", dir: "", entries: [] });
    for (let i = 0; i < 10; i++) store.cacheFile(file(`long-${i}.md`, "x".repeat(500 * 1024)));
    storage.attempts.length = 0;
    store.cacheFile(file("long-10.md", "x".repeat(500 * 1024)));
    expect(storage.attempts).toHaveLength(2);
    expect(storage.attempts[0]).toBeGreaterThan(5 * 1024 * 1024);
    expect(storage.attempts[1]).toBeLessThan(4096);
    expect(Object.keys(store.get().persistedFiles)).toHaveLength(0);
    expect(Object.keys(store.get().persistedListings)).toHaveLength(0);
    expect(Object.keys(store.get().files)).toHaveLength(11);
    expect(Object.keys(store.get().listings)).toHaveLength(1);
    expect(store.get().drafts).toEqual(drafts);
    expect(store.get().uncheckedSaves).toEqual(uncheckedSaves);
    storage.attempts.length = 0;
    for (let i = 1; i <= 5; i++) {
      store.saveDraft({ base: note, text: "- milk\n" + "x".repeat(i), updatedAt: i, phase: "draft" });
      expect(store.get().storageError).toBeNull();
    }
    expect(storage.attempts).toHaveLength(5);
    expect(storage.attempts.every((size) => size < 4096)).toBe(true);
    const reopened = new LibraryStore("quota", storage);
    expect(reopened.draft("notes", "todo.md")?.text).toBe("- milk\nxxxxx");
    expect(reopened.draft("notes", "pending.md")).toMatchObject(drafts[fileKey("notes", "pending.md")]!);
    expect(reopened.get().uncheckedSaves).toEqual(uncheckedSaves);
    // Fresh reads can be persisted again without resurrecting evicted bodies.
    storage.attempts.length = 0;
    store.cacheFile(file("fresh.md", "fresh read"));
    expect(storage.attempts).toHaveLength(1);
    expect(storage.attempts[0]).toBeLessThan(4096);
    const afterRead = new LibraryStore("quota", storage);
    expect(Object.keys(afterRead.get().files)).toEqual([fileKey("notes", "fresh.md")]);
    afterRead.form("name", "still small");
    expect(new LibraryStore("quota", storage).get().files[fileKey("notes", "fresh.md")]?.value.text).toBe("fresh read");
  });

  it("keeps drafts durable after reading several large files", () => {
    const storage = quotaStorage();
    const store = new LibraryStore("quota", storage);
    // Reading eleven long documents (each within the editor's limit) fills the
    // expendable file cache, which is stored beside the drafts.
    for (let i = 0; i < 11; i++) store.cacheFile(file(`long-${i}.md`, "x".repeat(READ_LIMIT - 1024)));
    // The person then edits a short note.
    const note = file("todo.md", "- milk\n");
    store.saveDraft({ base: note, text: "- milk\n- eggs\n", updatedAt: 1, phase: "draft" });
    expect(store.get().storageError).toBeNull();
    // The app is closed by the system and reopened.
    expect(new LibraryStore("quota", storage).draft("notes", "todo.md")?.text).toBe("- milk\n- eggs\n");
  });

  it("drops evicted caches even when storage is too full for drafts alone", () => {
    const storage = quotaStorage();
    const store = new LibraryStore("quota", storage);
    for (let i = 0; i < 9; i++) store.cacheFile(file(`long-${i}.md`, "x".repeat(500 * 1024)));
    // Another part of the app fills the rest of the origin's quota.
    storage.data.set("other", "y".repeat(5 * 1024 * 1024));
    storage.attempts.length = 0;
    const note = file("todo.md", "- milk\n");
    for (let i = 1; i <= 5; i++) {
      store.saveDraft({ base: note, text: "- milk\n" + "x".repeat(i), updatedAt: i, phase: "draft" });
      expect(store.get().storageError).not.toBeNull();
    }
    // The first edit tries the caches once; later edits never serialize them again.
    expect(Object.keys(store.get().persistedFiles)).toHaveLength(0);
    expect(storage.attempts[0]).toBeGreaterThan(4 * 1024 * 1024);
    expect(storage.attempts.slice(1).every((size) => size < 4096)).toBe(true);
    expect(store.draft("notes", "todo.md")?.text).toBe("- milk\nxxxxx");
    // A write failure must still recover once quota becomes available.
    storage.data.delete("other");
    store.cacheRoots([]);
    expect(store.get().storageError).toBeNull();
    expect(new LibraryStore("quota", storage).draft("notes", "todo.md")?.text).toBe("- milk\nxxxxx");
  });
});
