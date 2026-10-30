// SPDX-License-Identifier: Apache-2.0
import { Store } from "../../state/store";
import { preserveSavedText, readSavedJson, UnpreservedJson } from "../../state/savedJson";
import type { TransferDestination } from "../../protocol/types";
import { transferBytes } from "./storage";
interface ComposerState {
  mode: "file" | "text";
  text: string;
  name: string;
  fileName: string;
  fileSize: number;
  /** New for every file chosen: two files with one name and size are still two. */
  fileId?: string;
  destination: TransferDestination;
  error: string | null;
  revision?: number;
  submitted?: { jobId: string; revision: number };
}
const initial: ComposerState = {
  mode: "file", text: "", name: "note.md", fileName: "", fileSize: 0,
  destination: { kind: "inbox" }, error: null,
};
const RECOVERED = "Saved transfer text was damaged. A copy has been kept on this device for recovery. New changes can be saved again.";
const readError = (error: unknown) => error instanceof UnpreservedJson
  ? "Saved transfer text is damaged and could not be set aside. Keep the app open; new changes are only held in memory."
  : "Saved transfer text could not be read or preserved. Keep the app open; new changes are only held in memory until storage recovers.";

function load(key: string): { state: ComposerState; recovered: boolean } {
  const { value, recovered } = readSavedJson(localStorage, key);
  const v = value as ComposerState | null;
  // Older drafts with an artifact destination remain available to redirect.
  const valid = v && typeof v.text === "string" && typeof v.name === "string" &&
    typeof v.fileName === "string" && ["file", "text"].includes(v.mode) &&
    ["string", "undefined"].includes(typeof v.fileId) &&
    ["inbox", "artifact", "project_dir"].includes(v.destination?.kind);
  return { state: valid ? { ...initial, ...v, error: null } : initial, recovered };
}

type FileSelection = Pick<ComposerState, "fileName" | "fileSize"> & {
  fileId: string;
  mode?: "file";
};
export class TransferComposer extends Store<ComposerState> {
  private key: string;
  private unread = false;
  private pending: Partial<ComposerState> = {};
  readonly recovery = new Store<string | null>(null);
  constructor(profileId: string) {
    const key = `transfer-composer.${profileId}`;
    let state = initial, recovered = false;
    try { ({ state, recovered } = load(key)); }
    catch (error) { state = { ...initial, error: readError(error) }; }
    super(state);
    this.key = key;
    this.unread = state.error !== null;
    if (recovered) this.recovery.set(RECOVERED);
  }
  update(patch: Partial<ComposerState>) {
    const changes = { ...patch, revision: (this.get().revision ?? 0) + 1 };
    this.save({ ...this.get(), ...changes }, changes);
  }
  /** A saved file changes the active draft only while File is selected.
   * Returns whether the previous selection's bytes can be released. */
  chooseFile(file: FileSelection) {
    const s = this.get(), mode = file.mode ?? s.mode, wasUnread = this.unread;
    const changes = {
      ...file,
      // Text may have been sent from another view while this file was saved.
      // Keep its submission current, so completion still clears that text.
      revision: (s.revision ?? 0) + (mode === "file" ? 1 : 0),
    };
    const saved = this.save({ ...s, ...changes }, changes);
    // Unread storage may still name a legacy file or a recovered draft's file.
    // A failed write still names the previous choice after an app restart.
    return saved && !wasUnread;
  }
  submitted(jobId: string) {
    const changes = { submitted: { jobId, revision: this.get().revision ?? 0 } };
    this.save({ ...this.get(), ...changes }, changes);
  }
  /** Only clear the exact draft that completed, never a later selection. */
  completed(jobId: string): boolean {
    const s = this.get();
    if (s.submitted?.jobId !== jobId) return false;
    const same = s.submitted.revision === (s.revision ?? 0);
    const changes = {
      submitted: undefined,
      ...(same
        ? s.mode === "file"
          ? { fileName: "", fileSize: 0, fileId: undefined }
          : { text: "" }
        : {}),
    };
    this.save({ ...s, ...changes }, changes);
    return same;
  }
  persistNow(): boolean {
    return this.save(this.get());
  }
  private save(state: ComposerState, changes: Partial<ComposerState> = {}): boolean {
    if (this.unread) {
      this.pending = { ...this.pending, ...changes };
      try {
        const loaded = load(this.key);
        const saved = loaded.state;
        // Only fields actually edited while unread replace the recovered ones.
        // If the same text/file was changed, preserve the earlier draft first.
        if ((saved.text && this.pending.text !== undefined && this.pending.text !== saved.text) ||
            (saved.fileName && ((this.pending.fileName !== undefined && this.pending.fileName !== saved.fileName) ||
              (this.pending.fileId !== undefined && this.pending.fileId !== saved.fileId)))) {
          preserveSavedText(localStorage, `${this.key}.recovered`, JSON.stringify(saved));
          this.recovery.set("An earlier transfer draft was kept separately on this device for recovery. New changes can be saved again.");
        }
        // A successful reread introduces a draft the running upload never
        // saw. Keep that recovery newer than any submission made while unread,
        // even when restoring the other mode did not need a recovery copy.
        state = { ...saved, ...this.pending,
          revision: (saved.revision ?? 0) + (this.pending.revision ?? 0) + 1,
        };
        if (this.pending.submitted) state.submitted = {
          ...this.pending.submitted, revision: (saved.revision ?? 0) + this.pending.submitted.revision,
        };
        if (loaded.recovered) this.recovery.set(RECOVERED);
        this.unread = false;
        this.pending = {};
      } catch (error) {
        this.set({ ...state, error: readError(error) });
        return false;
      }
    }
    state = { ...state, error: null };
    try {
      localStorage.setItem(this.key, JSON.stringify(state));
    } catch {
      state.error =
        "This draft is only held in memory. Device storage is unavailable.";
    }
    this.set(state);
    return state.error === null;
  }
}
const composers = new Map<string, TransferComposer>();
/**
 * Where the bytes of a chosen file wait until they are sent. Each choice has a
 * place of its own, so a read for one choice can't return another's bytes.
 * Drafts saved before choices had one keep theirs at the shared place.
 */
export const composerFileKey = (profileId: string, fileId?: string) =>
  fileId ? `${profileId}:composer:${fileId}` : `${profileId}:composer`;
/**
 * Name a chosen file once its bytes are kept under `fileId`, so any view that
 * sees the new choice finds its bytes, and let go of the bytes it replaces.
 * The source (File or Text) changes only when `mode` is given.
 */
export function chooseComposerFile(
  profileId: string,
  file: FileSelection,
) {
  const composer = transferComposer(profileId),
    replaced = composer.get().fileId;
  if (composer.chooseFile(file)) {
    void transferBytes
      .remove(composerFileKey(profileId, replaced))
      .catch(() => undefined);
  }
}
export function transferComposer(profileId: string) {
  let store = composers.get(profileId);
  if (!store) {
    store = new TransferComposer(profileId);
    composers.set(profileId, store);
  }
  return store;
}
