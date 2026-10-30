// SPDX-License-Identifier: Apache-2.0
import { commandResult } from "../../host/commandResult";
import { CommandError, MAX_FRAME_BYTES, type HostSession } from "../../host/session";
import type { FileContent } from "../../protocol/types";
import { announce } from "../../design/Announcer";
import { forSave, READ_LIMIT } from "./model";
import type { FileDraft, LibraryStore } from "./drafts";

/** A merged draft must stay reopenable in the editor and fit a save command,
 * including JSON escaping. Reserve room for the command envelope. */
export function mergeFitsLimits(base: FileContent, text: string): boolean {
  const encode = (value: string) => new TextEncoder().encode(value).length;
  const saved = forSave(text, base.newline, base.text);
  return base.size <= READ_LIMIT &&
    encode(saved) + (base.has_bom ? 3 : 0) <= READ_LIMIT &&
    encode(JSON.stringify(saveCommand(base, saved))) <= MAX_FRAME_BYTES - 1024;
}
function saveCommand(base: FileContent, text: string) {
  return {
    command: "library_save" as const,
    root_id: base.root_id,
    path: base.path,
    base_revision: base.revision,
    text,
    newline: base.newline,
    bom: base.has_bom,
  };
}

export type SaveResult =
  | { kind: "saved"; file: FileContent }
  | { kind: "conflict" }
  | { kind: "unknown" };
export async function saveDraft(
  session: HostSession,
  store: LibraryStore,
  draft: FileDraft,
): Promise<SaveResult> {
  if (!session.online)
    throw new Error(
      "Connect to the computer before saving. Your draft stays on this device.",
    );
  draft = { ...draft, saveAttemptAt: Date.now() };
  const base = draft.base,
    text = forSave(draft.text, base.newline, base.text);
  store.saveDraft({ ...draft, phase: "sending" });
  try {
    const receipt = await session.command(
      {
        command: "library_save",
        root_id: base.root_id,
        path: base.path,
        base_revision: base.revision,
        text,
        newline: base.newline,
        bom: base.has_bom,
      },
      { label: `Save ${base.path}` },
    );
    const result = await commandResult(session, receipt);
    if (result?.kind !== "saved") {
      store.saveDraft({
        ...draft,
        phase: "unknown",
        recoveryId: receipt.command_id,
      });
      return { kind: "unknown" };
    }
    const outcome = result.outcome;
    if (outcome.outcome === "saved") {
      const file = {
        ...base,
        text,
        revision: outcome.revision,
        size: new TextEncoder().encode(text).length + (base.has_bom ? 3 : 0),
        modified_at: null,
      };
      store.cacheFile(file);
      store.removeDraft(base.root_id, base.path);
      announce("Saved on computer");
      return { kind: "saved", file };
    }
    if (outcome.outcome === "uncertain") {
      store.saveDraft({
        ...draft,
        phase: "unknown",
        recoveryId: outcome.recovery_id,
      });
      announce("The computer hasn’t confirmed this save");
      return { kind: "unknown" };
    }
    // A conflict leaves out a note too large to send with its result.
    const current =
      outcome.outcome === "conflict"
        ? (outcome.current ?? (await currentFile(session, base)))
        : null;
    // The computer's current file is the latest read, as it is for checkSave.
    if (current && current !== "too_large") store.cacheFile(current);
    store.saveDraft({
      ...draft,
      phase: "conflict",
      current: current === "too_large" ? null : current,
      currentTooLarge: current === "too_large" || undefined,
      currentAt: Date.now(),
    });
    announce("The file changed on the computer. Your draft is kept.");
    return { kind: "conflict" };
  } catch (e) {
    if (e instanceof CommandError && e.error.code !== "outcome_unknown") {
      store.saveDraft({ ...draft, phase: "draft" });
      throw e;
    }
    const pending = Object.values(session.commands.get())
      .filter(
        (c) =>
          c.body.command === "library_save" &&
          c.body.root_id === base.root_id &&
          c.body.path === base.path,
      )
      .at(-1);
    store.saveDraft({
      ...draft,
      phase: "unknown",
      recoveryId: e instanceof CommandError ? e.command?.id : pending?.id,
    });
    announce("The computer hasn’t confirmed this save");
    return { kind: "unknown" };
  }
}
/** The file as it is on the computer now, `null` when it is gone, or
 * `"too_large"` when it is too large to send to this device. */
async function currentFile(
  session: HostSession,
  base: FileContent,
): Promise<FileContent | null | "too_large"> {
  try {
    const { file } = await session.query({
      kind: "library_read",
      root_id: base.root_id,
      path: base.path,
    });
    return file;
  } catch (e) {
    if (e instanceof CommandError && e.error.code === "not_found") return null;
    if (e instanceof CommandError && e.error.code === "limit_exceeded")
      return "too_large";
    throw e;
  }
}
/** The draft as it is after an asynchronous read, so text typed during the
 * read is kept. `undefined` when it was discarded or replaced, re-based, or a
 * save was started meanwhile: the read no longer describes it. */
function stillCurrent(store: LibraryStore, draft: FileDraft): FileDraft | undefined {
  const latest = store.draft(draft.base.root_id, draft.base.path);
  return latest &&
    latest.startedAt === draft.startedAt &&
    latest.base.revision === draft.base.revision &&
    latest.saveAttemptAt === draft.saveAttemptAt &&
    latest.recoveryId === draft.recoveryId &&
    latest.movedTo === draft.movedTo &&
    latest.phase !== "sending"
    ? latest
    : undefined;
}
/** A draft whose base the computer still has: nothing to compare it with. */
function unconflicted(draft: FileDraft): FileDraft {
  return {
    ...draft,
    phase: "draft",
    current: undefined,
    currentTooLarge: undefined,
    currentAt: undefined,
  };
}
/** Bring the computer's newer version to a draft started from an older one,
 * so the two are compared before saving rather than when a save is refused. */
export async function compareWithComputer(
  session: HostSession,
  store: LibraryStore,
  draft: FileDraft,
): Promise<"conflict" | "unchanged" | "superseded"> {
  const file = await currentFile(session, draft.base);
  const latest = stillCurrent(store, draft);
  if (!latest) return "superseded";
  if (file && file !== "too_large") store.cacheFile(file);
  if (file && file !== "too_large" && file.revision === latest.base.revision) {
    if (latest.phase === "conflict") store.saveDraft(unconflicted(latest));
    return "unchanged";
  }
  store.saveDraft({
    ...latest,
    phase: "conflict",
    current: file === "too_large" ? null : file,
    currentTooLarge: file === "too_large" || undefined,
    currentAt: Date.now(),
  });
  return "conflict";
}
export async function checkSave(
  session: HostSession,
  store: LibraryStore,
  draft: FileDraft,
): Promise<SaveResult | { kind: "unchanged" } | { kind: "superseded" | "rebased" }> {
  const file = await currentFile(session, draft.base);
  const latest = stillCurrent(store, draft);
  if (!latest) return { kind: "superseded" };
  if (file && file !== "too_large") {
    store.cacheFile(file);
    if (
      file.text === forSave(latest.text, latest.base.newline, latest.base.text) &&
      file.has_bom === latest.base.has_bom
    ) {
      store.removeDraft(file.root_id, file.path);
      announce("The computer confirms the saved text.");
      return { kind: "saved", file };
    }
    if (
      file.text === forSave(draft.text, draft.base.newline, draft.base.text) &&
      file.has_bom === draft.base.has_bom
    ) {
      store.saveDraft({
        ...unconflicted(latest), base: file, recoveryId: undefined, saveAttemptAt: undefined,
      });
      announce("The earlier save is confirmed. Your newer edits are kept on this device.");
      return { kind: "rebased" };
    }
    if (file.revision === latest.base.revision) {
      store.saveDraft({ ...unconflicted(latest), recoveryId: undefined });
      return { kind: "unchanged" };
    }
  }
  // Changed, gone, or too large to send here: none of those is this draft.
  store.saveDraft({
    ...latest,
    phase: "conflict",
    current: file === "too_large" ? null : file,
    currentTooLarge: file === "too_large" || undefined,
    currentAt: Date.now(),
  });
  return { kind: "conflict" };
}
