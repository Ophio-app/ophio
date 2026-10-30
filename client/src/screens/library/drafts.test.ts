// SPDX-License-Identifier: Apache-2.0
import { beforeEach, describe, expect, it } from "vitest";
import { LibraryStore, fileKey } from "./drafts";
import type { FileContent } from "../../protocol/types";
export const base: FileContent = {
  root_id: "r",
  path: "notes.md",
  text: "before\r\n",
  revision: "base-hash",
  encoding: "utf8",
  newline: "cr_lf",
  has_bom: true,
  size: 11,
  modified_at: 123,
};
beforeEach(() => localStorage.clear());
describe("phone drafts", () => {
  it("persists text, original revision, newline and timestamp through restart", () => {
    const a = new LibraryStore("p");
    a.saveDraft({ base, text: "after\n", updatedAt: 321, phase: "draft" });
    expect(new LibraryStore("p").draft("r", "notes.md")).toEqual({
      base,
      text: "after\n",
      updatedAt: 321,
      phase: "draft",
      reviewMerge: false,
    });
  });
  it("isolates profiles and roots and uses unambiguous path keys", () => {
    const a = new LibraryStore("p");
    a.saveDraft({ base, text: "after", updatedAt: 1, phase: "draft" });
    expect(new LibraryStore("other").draft("r", "notes.md")).toBeNull();
    expect(a.draft("other", "notes.md")).toBeNull();
    expect(fileKey("a/b", "c")).not.toBe(fileKey("a", "b/c"));
  });
  it("recovers an interrupted save as unknown", () => {
    new LibraryStore("p").saveDraft({
      base,
      text: "after",
      updatedAt: 1,
      phase: "sending",
      recoveryId: "receipt-1",
    });
    expect(new LibraryStore("p").draft("r", "notes.md")?.phase).toBe("unknown");
  });
  it("does not reset the draft base when a newer computer file is cached", () => {
    const s = new LibraryStore("p");
    s.saveDraft({ base, text: "after", updatedAt: 1, phase: "draft" });
    s.cacheFile({ ...base, text: "newer", revision: "new-hash" });
    expect(s.draft("r", "notes.md")?.base.revision).toBe("base-hash");
  });
  it("retains in-memory edits but exposes persistence failure", () => {
    const s = new LibraryStore("p", {
      getItem: () => null,
      setItem: () => {
        throw new Error("quota");
      },
    });
    s.saveDraft({ base, text: "precious", updatedAt: 1, phase: "draft" });
    expect(s.draft("r", "notes.md")?.text).toBe("precious");
    expect(s.get().storageError).toContain("full");
  });
  it("discard only removes the named draft", () => {
    const s = new LibraryStore("p");
    s.saveDraft({ base, text: "one", updatedAt: 1, phase: "draft" });
    s.saveDraft({
      base: { ...base, path: "second.md" },
      text: "two",
      updatedAt: 1,
      phase: "draft",
    });
    s.removeDraft("r", "notes.md");
    expect(new LibraryStore("p").draft("r", "notes.md")).toBeNull();
    expect(s.draft("r", "second.md")?.text).toBe("two");
  });
  it("discarding unconfirmed text preserves its unresolved status across restart and move", () => {
    const s = new LibraryStore("discard");
    s.saveDraft({ base, text: "unsent", updatedAt: 42, phase: "unknown", recoveryId: "r1" });
    s.discardDraft("r", "notes.md");
    s.moveFile("r", "notes.md", "new.md");
    const restored = new LibraryStore("discard");
    expect(restored.draft("r", "notes.md")).toBeNull();
    expect(restored.get().uncheckedSaves[fileKey("r", "notes.md")]).toEqual({
      rootId: "r", path: "notes.md", at: 42, recoveryId: "r1", movedTo: "new.md",
    });
    expect(localStorage.getItem(restored.key)).not.toContain("unsent");
    restored.checkedSave("r", "notes.md");
    expect(new LibraryStore("discard").get().uncheckedSaves).toEqual({});
  });
  it("moves a draft with its base, preserves edits, and drops dead recent entries", () => {
    const s = new LibraryStore("move");
    s.saveDraft({ base, text: "unsent", updatedAt: 42, phase: "draft" });
    s.remember("r", "notes.md");
    s.favourite("r", "notes.md");
    s.cacheFile(base);
    s.moveFile("r", "notes.md", "folder/notes.md");
    const restored = new LibraryStore("move");
    expect(restored.draft("r", "folder/notes.md")).toMatchObject({
      base: { ...base, path: "folder/notes.md" }, text: "unsent", updatedAt: 42,
    });
    expect(restored.draft("r", "notes.md")).toBeNull();
    expect(restored.get().recent).toEqual([]);
    expect(restored.get().files[fileKey("r", "notes.md")]).toBeUndefined();
    expect(restored.get().favourites[0]?.path).toBe("folder/notes.md");
  });
  it.each(["draft", "unknown"] as const)("preserves a %s draft that cannot follow the file", (phase) => {
    const s = new LibraryStore("collision");
    s.saveDraft({ base, text: "old edit", updatedAt: 42, phase });
    if (phase === "draft") s.saveDraft({ base: { ...base, path: "new.md" }, text: "other edit", updatedAt: 43, phase });
    s.moveFile("r", "notes.md", "new.md");
    s.moveFile("r", "new.md", "final.md");
    expect(new LibraryStore("collision").draft("r", "notes.md")).toMatchObject({ text: "old edit", movedTo: "final.md", phase });
    if (phase === "draft") expect(s.draft("r", "final.md")?.text).toBe("other edit");
  });
});
