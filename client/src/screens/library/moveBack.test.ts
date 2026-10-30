// SPDX-License-Identifier: Apache-2.0
import { beforeEach, describe, expect, it } from "vitest";
import { LibraryStore } from "./drafts";
import type { FileContent } from "../../protocol/types";

const base: FileContent = {
  root_id: "r",
  path: "notes.md",
  text: "before\n",
  revision: "base-hash",
  encoding: "utf8",
  newline: "lf",
  has_bom: false,
  size: 7,
  modified_at: 123,
};
beforeEach(() => localStorage.clear());
describe("a file that returns to its draft's location", () => {
  it("lets an unconfirmed draft follow the file back after a rename is undone", () => {
    const s = new LibraryStore("move-back");
    s.saveDraft({ base, text: "unsent edit\n", updatedAt: 42, phase: "unknown", recoveryId: "c1" });
    // An unconfirmed draft cannot follow a rename; it records where the file went.
    s.moveFile("r", "notes.md", "note.md");
    expect(s.draft("r", "notes.md")?.movedTo).toBe("note.md");
    // The person renames it back: the file is at the draft's own path again.
    s.moveFile("r", "note.md", "notes.md");
    const draft = new LibraryStore("move-back").draft("r", "notes.md");
    expect(draft?.text).toBe("unsent edit\n");
    // Otherwise File.tsx treats it as stranded forever: not editable, not saveable,
    // and MovedDrafts lists it as "a draft from this file's old location".
    expect(draft?.movedTo).toBeUndefined();
  });
  it("releases a stranded draft when its file comes back, keeping the other draft stranded", () => {
    const s = new LibraryStore("move-back-2");
    s.saveDraft({ base, text: "mine\n", updatedAt: 1, phase: "draft" });
    s.saveDraft({ base: { ...base, path: "other.md", revision: "other" }, text: "theirs\n", updatedAt: 2, phase: "draft" });
    s.moveFile("r", "notes.md", "other.md");
    s.moveFile("r", "other.md", "notes.md");
    expect(s.draft("r", "notes.md")).toMatchObject({ text: "mine\n", movedTo: undefined });
    expect(s.draft("r", "other.md")).toMatchObject({ text: "theirs\n", movedTo: "notes.md" });
  });
});
