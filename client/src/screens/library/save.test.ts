// SPDX-License-Identifier: Apache-2.0
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  CommandReceipt,
  FileContent,
  SaveOutcome,
} from "../../protocol/types";
import { CommandError, HostSession } from "../../host/session";
import { fileKey, LibraryStore, type FileDraft } from "./drafts";
import { saveDraft, checkSave, compareWithComputer, mergeFitsLimits } from "./save";
import { READ_LIMIT } from "./model";
const base: FileContent = {
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
function setup() {
  const session = new HostSession("p", {
    connect() {},
    disconnect() {},
    listen: () => () => {},
    send: () => true,
  });
  session.link.set({ state: "online", since: 0 });
  const store = new LibraryStore("p");
  const draft: FileDraft = {
    base,
    text: "after\n",
    updatedAt: 321,
    phase: "draft",
  };
  store.saveDraft(draft);
  return { session, store, draft };
}
const receipt = (outcome: SaveOutcome): CommandReceipt => ({
  command_id: "c1",
  status: "accepted",
  seq: 1,
  error: null,
  result: { kind: "saved", outcome },
});
function deferRead(session: HostSession) {
  let reply!: (file: FileContent) => void;
  let fail!: (error: Error) => void;
  vi.spyOn(session, "query").mockImplementationOnce(() => new Promise((resolve, reject) => {
    reply = (file) => resolve({ kind: "library_read", file });
    fail = reject;
  }));
  return {
    reply: (file: FileContent) => reply(file),
    fail: (error: Error) => fail(error),
  };
}
beforeEach(() => localStorage.clear());
describe("merged draft limits", () => {
  it("keeps both the merge and its base within the editor limit, counting UTF-8 and BOM bytes", () => {
    expect(mergeFitsLimits(base, "x".repeat(READ_LIMIT - 3))).toBe(true);
    expect(mergeFitsLimits(base, "x".repeat(READ_LIMIT - 2))).toBe(false);
    expect(mergeFitsLimits(base, "界".repeat(180_000))).toBe(false);
    expect(mergeFitsLimits(base, "\n".repeat(300_000))).toBe(false);
    expect(mergeFitsLimits({ ...base, size: READ_LIMIT + 1 }, "small merged text")).toBe(false);
  });
  it("checks escaped save-command bytes even when the text fits the editor", () => {
    const text = "\u0001".repeat(180_000);
    expect(new TextEncoder().encode(text).length).toBeLessThan(READ_LIMIT);
    expect(mergeFitsLimits(base, text)).toBe(false);
    expect(mergeFitsLimits(base, "a quoted line: \"hello\"\r\n")).toBe(true);
  });
});
describe("revision guarded saves", () => {
  it.each(["saved", "unchanged", "conflict", "missing"] as const)(
    "keeps edits made while checking an uncertain save after a %s reply, including after reload",
    async (outcome) => {
      const { session, store, draft } = setup();
      const checking: FileDraft = {
        ...draft,
        text: "first edit\n",
        phase: "unknown",
        recoveryId: "recovery-1",
        saveAttemptAt: 400,
      };
      store.saveDraft(checking);
      const { reply, fail } = deferRead(session);
      const send = vi.spyOn(session, "command");
      const pending = checkSave(session, store, checking);
      const newer = { ...checking, text: "second edit\n", updatedAt: 500 };
      store.saveDraft(newer);
      const file = outcome === "saved"
        ? { ...base, text: "first edit\r\n", revision: "saved-hash" }
        : outcome === "unchanged"
          ? base
          : { ...base, text: "computer edit\r\n", revision: "computer-hash" };
      if (outcome === "missing") {
        fail(new CommandError({ code: "not_found", message: "File missing" }, null));
      } else reply(file);
      const result = await pending;

      for (const persisted of [store, new LibraryStore("p")]) {
        expect(persisted.draft("r", "notes.md")).toMatchObject({
          text: newer.text,
          updatedAt: newer.updatedAt,
          base: outcome === "saved" ? file : base,
          phase: outcome === "saved" || outcome === "unchanged" ? "draft" : "conflict",
          ...(outcome === "conflict" || outcome === "missing"
            ? { current: outcome === "missing" ? null : file }
            : {}),
        });
        if (outcome === "saved" || outcome === "unchanged") {
          expect(persisted.draft("r", "notes.md")?.recoveryId).toBeUndefined();
        }
      }
      expect(result.kind).toBe(outcome === "saved" ? "rebased" : outcome === "unchanged" ? "unchanged" : "conflict");
      expect(send).not.toHaveBeenCalled();
      if (outcome === "saved") {
        send.mockResolvedValue(receipt({ outcome: "saved", revision: "second-save-hash" }));
        await saveDraft(session, store, new LibraryStore("p").draft("r", "notes.md")!);
        expect(send).toHaveBeenCalledWith(expect.objectContaining({
          base_revision: "saved-hash",
          text: "second edit\r\n",
          bom: true,
        }), expect.anything());
      }
    },
  );
  it.each(["saved", "unchanged", "conflict", "missing"] as const)(
    "does not resurrect a discarded draft after a late %s reply",
    async (outcome) => {
      const { session, store, draft } = setup();
      const checking: FileDraft = { ...draft, phase: "unknown", recoveryId: "recovery-1" };
      store.saveDraft(checking);
      const { reply, fail } = deferRead(session);
      const pending = checkSave(session, store, checking);
      store.discardDraft("r", "notes.md");
      if (outcome === "missing") {
        fail(new CommandError({ code: "not_found", message: "File missing" }, null));
      } else {
        reply(outcome === "saved"
          ? { ...base, text: "after\r\n", revision: "saved-hash" }
          : outcome === "unchanged" ? base : { ...base, text: "other", revision: "other-hash" });
      }
      expect(await pending).toEqual({ kind: "superseded" });
      expect(store.draft("r", "notes.md")).toBeNull();
      expect(new LibraryStore("p").draft("r", "notes.md")).toBeNull();
    },
  );
  it("does not change a replacement draft when an older check replies", async () => {
    const { session, store, draft } = setup();
    const { reply } = deferRead(session);
    const pending = checkSave(session, store, draft);
    const replacement = { ...draft, base: { ...base, revision: "replacement" }, text: "new draft" };
    store.saveDraft(replacement);
    reply({ ...base, text: "after\r\n", revision: "old-save" });
    expect(await pending).toEqual({ kind: "superseded" });
    expect(store.draft("r", "notes.md")).toEqual(replacement);
    expect(new LibraryStore("p").draft("r", "notes.md")).toMatchObject(replacement);
  });
  it("does not clear a moved uncertain draft when its old path's check replies", async () => {
    const { session, store, draft } = setup();
    const checking: FileDraft = { ...draft, phase: "unknown", recoveryId: "recovery-1" };
    store.saveDraft(checking);
    const { reply } = deferRead(session);
    const pending = checkSave(session, store, checking);
    store.moveFile("r", "notes.md", "moved.md");
    reply({ ...base, text: "after\r\n", revision: "saved-hash" });
    expect(await pending).toEqual({ kind: "superseded" });
    expect(new LibraryStore("p").draft("r", "notes.md")).toMatchObject({
      ...checking,
      movedTo: "moved.md",
    });
  });

  it("quotes the original revision and preserves BOM and CRLF", async () => {
    const { session, store, draft } = setup(),
      send = vi
        .spyOn(session, "command")
        .mockResolvedValue(receipt({ outcome: "saved", revision: "new-hash" }));
    await saveDraft(session, store, draft);
    expect(send).toHaveBeenCalledWith(
      expect.objectContaining({
        base_revision: "base-hash",
        text: "after\r\n",
        newline: "cr_lf",
        bom: true,
      }),
      expect.anything(),
    );
    expect(store.draft("r", "notes.md")).toBeNull();
  });
  it("keeps all three versions after conflict, without retrying", async () => {
    const { session, store, draft } = setup(),
      current = { ...base, text: "computer", revision: "computer-hash" },
      send = vi
        .spyOn(session, "command")
        .mockResolvedValue(receipt({ outcome: "conflict", current }));
    expect(await saveDraft(session, store, draft)).toEqual({
      kind: "conflict",
    });
    expect(send).toHaveBeenCalledTimes(1);
    expect(store.draft("r", "notes.md")).toMatchObject({
      base,
      text: "after\n",
      current,
      phase: "conflict",
    });
    // Like the read in checkSave, the computer’s current file is now the cached read.
    expect(store.get().files[fileKey("r", "notes.md")]?.value).toEqual(current);
  });
  it("reads a conflicting note the computer left out of the result", async () => {
    const { session, store, draft } = setup(),
      current = { ...base, text: "computer", revision: "computer-hash" };
    vi.spyOn(session, "command").mockResolvedValue(receipt({ outcome: "conflict", current: null }));
    const read = vi.spyOn(session, "query").mockResolvedValue({ kind: "library_read", file: current });
    expect(await saveDraft(session, store, draft)).toEqual({ kind: "conflict" });
    expect(read).toHaveBeenCalledWith({ kind: "library_read", root_id: "r", path: "notes.md" });
    expect(store.draft("r", "notes.md")).toMatchObject({ current, phase: "conflict" });
    expect(store.draft("r", "notes.md")?.currentTooLarge).toBeUndefined();
  });
  it("keeps the draft in conflict when the computer's note is too large to send", async () => {
    const { session, store, draft } = setup();
    vi.spyOn(session, "command").mockResolvedValue(receipt({ outcome: "conflict", current: null }));
    vi.spyOn(session, "query").mockRejectedValue(
      new CommandError({ code: "limit_exceeded", message: "This reply is too large to send." }, null),
    );
    expect(await saveDraft(session, store, draft)).toEqual({ kind: "conflict" });
    expect(store.draft("r", "notes.md")).toMatchObject({ text: "after\n", current: null, currentTooLarge: true, phase: "conflict" });
    expect(new LibraryStore("p").draft("r", "notes.md")).toMatchObject({ current: null, currentTooLarge: true });
  });
  it("checks a save against a note too large to send as a conflict, and clears that once it is gone", async () => {
    const { session, store, draft } = setup();
    const read = vi.spyOn(session, "query").mockRejectedValue(
      new CommandError({ code: "limit_exceeded", message: "This reply is too large to send." }, null),
    );
    expect(await checkSave(session, store, { ...draft, phase: "unknown" })).toEqual({ kind: "conflict" });
    const conflict = store.draft("r", "notes.md")!;
    expect(conflict).toMatchObject({ phase: "conflict", current: null, currentTooLarge: true });
    read.mockRejectedValue(new CommandError({ code: "not_found", message: "Gone" }, null));
    expect(await checkSave(session, store, conflict)).toEqual({ kind: "conflict" });
    expect(store.draft("r", "notes.md")).toMatchObject({ phase: "conflict", current: null });
    expect(store.draft("r", "notes.md")?.currentTooLarge).toBeUndefined();
  });
  it("keeps uncertain text and its recovery id, then reconciles the actual bytes", async () => {
    const { session, store, draft } = setup();
    vi.spyOn(session, "command").mockResolvedValue(
      receipt({ outcome: "uncertain", recovery_id: "recovery-1" }),
    );
    expect(await saveDraft(session, store, draft)).toEqual({ kind: "unknown" });
    expect(store.draft("r", "notes.md")?.recoveryId).toBe("recovery-1");
    expect(store.draft("r", "notes.md")?.saveAttemptAt).toBeGreaterThan(draft.updatedAt);
    expect(new LibraryStore("p").draft("r", "notes.md")?.saveAttemptAt).toBe(store.draft("r", "notes.md")?.saveAttemptAt);
    vi.spyOn(session, "query").mockResolvedValue({
      kind: "library_read",
      file: { ...base, text: "after\r\n", revision: "new" },
    });
    // As File does: Check again starts from the stored, unconfirmed draft.
    expect((await checkSave(session, store, store.draft("r", "notes.md")!)).kind).toBe("saved");
    expect(store.draft("r", "notes.md")).toBeNull();
  });
  it("leaves a newer unconfirmed save alone when an older check's read returns", async () => {
    const { session, store, draft } = setup();
    vi.spyOn(session, "command").mockResolvedValue(
      receipt({ outcome: "uncertain", recovery_id: "recovery-1" }),
    );
    await saveDraft(session, store, draft);
    const first = store.draft("r", "notes.md")!;
    let answer: (v: { kind: "library_read"; file: FileContent }) => void = () => undefined;
    vi.spyOn(session, "query").mockReturnValueOnce(new Promise((resolve) => { answer = resolve; }));
    const pending = checkSave(session, store, first);
    // Meanwhile the draft went back to "draft" and another save was sent and
    // left unconfirmed.
    store.saveDraft({ ...first, phase: "draft", recoveryId: undefined });
    const now = vi.spyOn(Date, "now").mockReturnValue(first.saveAttemptAt! + 5000);
    vi.spyOn(session, "command").mockResolvedValue(
      receipt({ outcome: "uncertain", recovery_id: "recovery-2" }),
    );
    expect(await saveDraft(session, store, store.draft("r", "notes.md")!)).toEqual({ kind: "unknown" });
    now.mockRestore();
    // The first read now reports the version the draft started from.
    answer({ kind: "library_read", file: base });
    expect(await pending).toEqual({ kind: "superseded" });
    expect(store.draft("r", "notes.md")).toMatchObject({ phase: "unknown", recoveryId: "recovery-2" });
  });
  it("keeps a later confirmed save's file when an older check or comparison's read returns", async () => {
    const { session, store, draft } = setup();
    const answers: ((v: { kind: "library_read"; file: FileContent }) => void)[] = [];
    vi.spyOn(session, "query").mockImplementation(
      () => new Promise((resolve) => answers.push(resolve)),
    );
    const comparing = compareWithComputer(session, store, draft);
    vi.spyOn(session, "command").mockResolvedValue(
      receipt({ outcome: "uncertain", recovery_id: "recovery-1" }),
    );
    await saveDraft(session, store, store.draft("r", "notes.md")!);
    const checking = checkSave(session, store, store.draft("r", "notes.md")!);
    // Meanwhile a second check confirmed the original version, and the next
    // save went through.
    store.saveDraft({ ...store.draft("r", "notes.md")!, phase: "draft", recoveryId: undefined });
    const now = vi.spyOn(Date, "now").mockReturnValue(Date.now() + 5000);
    vi.spyOn(session, "command").mockResolvedValue(
      receipt({ outcome: "saved", revision: "new-hash" }),
    );
    expect((await saveDraft(session, store, store.draft("r", "notes.md")!)).kind).toBe("saved");
    now.mockRestore();
    const saved = store.get().files[fileKey("r", "notes.md")]?.value;
    expect(saved?.revision).toBe("new-hash");
    // Both earlier reads now report the version the draft started from.
    for (const answer of answers) answer({ kind: "library_read", file: base });
    expect(await comparing).toBe("superseded");
    expect(await checking).toEqual({ kind: "superseded" });
    expect(store.get().files[fileKey("r", "notes.md")]?.value).toEqual(saved);
    expect(store.draft("r", "notes.md")).toBeNull();
  });
  it("clears a conflict when Compare finds the computer back at the draft's starting version", async () => {
    const { session, store, draft } = setup();
    const changed = { ...base, text: "theirs\r\n", revision: "their-hash" };
    const read = vi
      .spyOn(session, "query")
      .mockResolvedValue({ kind: "library_read", file: changed });
    expect(await compareWithComputer(session, store, draft)).toBe("conflict");
    expect(store.draft("r", "notes.md")).toMatchObject({ phase: "conflict", current: changed });
    // The phone text changed while the conflict was open, then the computer
    // went back to the version the draft started from.
    store.saveDraft({ ...store.draft("r", "notes.md")!, text: "after, again\n" });
    read.mockResolvedValue({ kind: "library_read", file: base });
    expect(await compareWithComputer(session, store, store.draft("r", "notes.md")!)).toBe("unchanged");
    const cleared = store.draft("r", "notes.md")!;
    expect(cleared).toMatchObject({ phase: "draft", text: "after, again\n" });
    expect(cleared.current).toBeUndefined();
    expect(cleared.currentAt).toBeUndefined();
    expect(store.get().files[fileKey("r", "notes.md")]?.value).toEqual(base);
  });
  it("does not treat unchanged computer bytes as a successful save", async () => {
    const { session, store, draft } = setup();
    vi.spyOn(session, "query").mockResolvedValue({
      kind: "library_read",
      file: base,
    });
    expect((await checkSave(session, store, draft)).kind).toBe("unchanged");
    expect(store.draft("r", "notes.md")?.text).toBe(draft.text);
  });
  it("preserves the draft on permission denial and refuses offline saves", async () => {
    const { session, store, draft } = setup();
    vi.spyOn(session, "command").mockRejectedValue(
      new CommandError(
        { code: "permission_denied", message: "Read only" },
        null,
      ),
    );
    await expect(saveDraft(session, store, draft)).rejects.toThrow("Read only");
    expect(store.draft("r", "notes.md")?.phase).toBe("draft");
    session.link.set({
      state: "offline",
      reason: "gone",
      since: 0,
      retryAt: null,
    });
    await expect(saveDraft(session, store, draft)).rejects.toThrow("Connect");
  });
});
