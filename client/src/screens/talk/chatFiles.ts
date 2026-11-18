// SPDX-License-Identifier: Apache-2.0
//
// Files attached to a chat message. The phone keeps each chosen file with
// the message's draft until the computer has the message: its bytes in this
// device's source storage, its name, size and type on the draft, and, once
// uploaded, the computer's id for it, so trying again never sends the same
// bytes twice. The phone chooses only the name people see; the computer
// chooses where the copy goes and gives the agent a read-only copy.

import type { App } from "../../app/context";
import type { Draft, UiStore } from "../../app/ui";
import { upload, TransferError } from "../../host/transfer";
import type { AnnotationBundle, AttachedFile } from "../../protocol/types";
import { uuidv7 } from "../../protocol/ids";
import { blobs } from "../../state/annotations";
import { Store, useStore } from "../../state/store";

/** The computer takes files on chat messages. */
export const CHAT_FILES_FEATURE = "chat_files_v1";

// The computer's limits (protocol annotation.rs), checked here first so a
// refusal comes before anything is uploaded. Decimal, as phones show sizes.
/** One file: a large document, a recording, a build log or a small archive. */
export const MAX_FILE_BYTES = 100_000_000;
/** Files on one message. */
export const MAX_FILES = 10;
/** All the files on one message. */
export const MAX_MESSAGE_BYTES = 250_000_000;
/** A name kept to what people read: the computer shortens it further when it tells the agent. */
const MAX_SHOWN_NAME = 255;
/** The computer's limit on a transfer's name is 200 bytes. */
const MAX_UPLOAD_NAME_BYTES = 150;

export interface FileDraft {
  /** This file on this draft. */
  key: string;
  /** As the person's device named it, made one line. */
  name: string;
  size: number;
  mediaType: string;
  /** Where its bytes wait on this device (state/annotations' source storage). */
  blobKey: string;
  /** Set once the computer has it, with the bytes' fingerprint. */
  artifactId?: string;
  sha256?: string;
  /** The upload under way, so a dropped one continues where it stopped. */
  transferId?: string;
}

/** A size as phones say it: "820 KB", "4.5 MB", "1 GB". */
export function amount(bytes: number): string {
  if (bytes < 1000) return `${bytes} byte${bytes === 1 ? "" : "s"}`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1000, unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit++;
  }
  const text = value < 10 ? value.toFixed(1).replace(/\.0$/, "") : Math.round(value).toString();
  return `${text} ${units[unit]}`;
}

// Control characters and the marks that reorder text, which could make one
// name read as another.
// eslint-disable-next-line no-control-regex
const UNSAFE = /[\u0000-\u001f\u007f-\u009f؜‎‏‪-‮⁦-⁩]/g;

/** The name people see: one line, the last part of any path, within limits. */
export function shownName(raw: string): string {
  const last = raw.replace(UNSAFE, " ").split(/[\\/]/).filter((p) => p.trim()).at(-1) ?? "";
  const one = last.replace(/\s+/g, " ").trim();
  const chars = [...one];
  return (chars.length > MAX_SHOWN_NAME ? chars.slice(0, MAX_SHOWN_NAME).join("") : one) || "file";
}

const RESERVED = /^(con|prn|aux|nul|conin\$|conout\$|com[1-9¹²³]|lpt[1-9¹²³]) *(\.|$)/i;
const encoder = new TextEncoder();

/** A single safe file name for the transfer. The computer names its own
 * copy; this only has to pass as a transfer's name. */
export function uploadName(raw: string): string {
  let name = shownName(raw).replace(/[/\\:<>"|?*]/g, "_");
  const dot = name.lastIndexOf(".");
  const ext = dot > 0 && name.length - dot <= 16 ? name.slice(dot) : "";
  let stem = ext ? name.slice(0, dot) : name;
  while (encoder.encode(stem + ext).length > MAX_UPLOAD_NAME_BYTES) stem = [...stem].slice(0, -1).join("");
  name = (stem + ext).replace(/[ .]+$/, "").replace(/^\.+/, "");
  if (!name || RESERVED.test(name)) name = `_${name || "file"}`;
  return name;
}

/** A media type the computer keeps; anything else is a plain file. */
function mediaTypeOf(file: File): string {
  return /^[a-z0-9][\w.+-]*\/[a-z0-9][\w.+-]*$/i.test(file.type) && file.type.length <= 100 ? file.type.toLowerCase() : "application/octet-stream";
}

/** Which of `picked` fit beside `held`, and in plain words why any don't. */
export function fitting<T extends { name: string; size: number }>(held: readonly { size: number }[], picked: readonly T[]): { take: T[]; refused: string | null } {
  const take: T[] = [];
  const said: string[] = [];
  let total = held.reduce((a, f) => a + f.size, 0);
  for (const file of picked) {
    if (file.size > MAX_FILE_BYTES) {
      said.push(`“${shownName(file.name)}” is ${amount(file.size)}. Files up to ${amount(MAX_FILE_BYTES)} can be attached.`);
    } else if (held.length + take.length >= MAX_FILES) {
      said.push(`A message can carry up to ${MAX_FILES} files. Send the rest in another message.`);
      break;
    } else if (total + file.size > MAX_MESSAGE_BYTES) {
      said.push(`“${shownName(file.name)}” would bring this message to ${amount(total + file.size)}. One message can carry up to ${amount(MAX_MESSAGE_BYTES)}; send it in another message.`);
    } else {
      take.push(file);
      total += file.size;
    }
  }
  return { take, refused: said.length ? said.join(" ") : null };
}

/** Keeps `picked` with the draft. Returns what was refused, if anything. */
export async function addFiles(ui: UiStore, projectId: string, taskId: string | null, picked: readonly File[]): Promise<string | null> {
  const held = ui.draft(projectId, taskId)?.files ?? [];
  const { take, refused } = fitting(held, picked);
  const added: FileDraft[] = [];
  try {
    for (const file of take) {
      const blobKey = await blobs.put(file);
      added.push({ key: uuidv7(Date.now()), name: shownName(file.name), size: file.size, mediaType: mediaTypeOf(file), blobKey });
    }
  } catch {
    await Promise.all(added.map((f) => blobs.remove(f.blobKey).catch(() => undefined)));
    return "This device couldn’t keep the files to send. Free some storage and try again.";
  }
  if (added.length) {
    // Read again: something may have been added while the bytes were stored.
    const now = ui.draft(projectId, taskId)?.files ?? [];
    const { take: kept, refused: late } = fitting(now, added);
    await Promise.all(added.filter((f) => !kept.includes(f)).map((f) => blobs.remove(f.blobKey).catch(() => undefined)));
    if (kept.length) ui.saveDraft(projectId, taskId, { files: [...now, ...kept] });
    if (late) return [refused, late].filter(Boolean).join(" ");
  }
  return refused;
}

/** Every file's bytes still wanted by a draft, saved or current. */
function wanted(ui: UiStore): Set<string> {
  const s = ui.get();
  const all: Draft[] = [...Object.values(s.drafts), ...Object.values(s.savedDrafts).flat()];
  return new Set(all.flatMap((d) => (d.files ?? []).map((f) => f.blobKey)));
}

/** Lets go of the bytes of `files` no draft holds any more. */
export async function forget(ui: UiStore, files: readonly FileDraft[]): Promise<void> {
  const keep = wanted(ui);
  await Promise.all(files.filter((f) => !keep.has(f.blobKey)).map((f) => blobs.remove(f.blobKey).catch(() => undefined)));
}

/** Takes one file off the draft. */
export function removeFile(ui: UiStore, projectId: string, taskId: string | null, key: string): void {
  const files = ui.draft(projectId, taskId)?.files ?? [];
  const gone = files.filter((f) => f.key === key);
  ui.saveDraft(projectId, taskId, { files: files.filter((f) => f.key !== key) });
  void forget(ui, gone);
}

/** Takes the files sent, by key, off the draft: anything added since stays. */
export function consumeFiles(ui: UiStore, projectId: string, taskId: string | null, sent: readonly string[] | undefined): void {
  if (!sent?.length) return;
  const files = ui.draft(projectId, taskId)?.files ?? [];
  const gone = files.filter((f) => sent.includes(f.key));
  if (!gone.length) return;
  ui.saveDraft(projectId, taskId, { files: files.filter((f) => !sent.includes(f.key)) });
  void forget(ui, gone);
}

/** How far each file's upload has got, by key, while it goes. */
export const fileProgress = new Store<Record<string, number>>({});
const progressOf = (key: string) => (s: Record<string, number>) => s[key];
export const useFileProgress = (key: string): number | undefined => useStore(fileProgress, progressOf(key));
const setProgress = (key: string, fraction: number | null) =>
  fileProgress.set((s) => {
    const next = { ...s };
    if (fraction === null) delete next[key];
    else next[key] = fraction;
    return next;
  });

/**
 * Uploads every file not yet on the computer, recording each id on the draft
 * as it arrives, and returns the files as the message names them. Throws
 * what stopped it; files already uploaded stay recorded.
 */
export async function uploadFiles(
  app: App,
  projectId: string,
  taskId: string | null,
  files: readonly FileDraft[],
  progress: (detail: string) => void,
): Promise<AttachedFile[]> {
  const { platform, profile, session, ui } = app;
  const ctx = { platform, profileId: profile.profileId, session };
  const record = (key: string, patch: Partial<FileDraft>) => {
    const now = ui.draft(projectId, taskId)?.files;
    if (now?.some((f) => f.key === key)) ui.saveDraft(projectId, taskId, { files: now.map((f) => (f.key === key ? { ...f, ...patch } : f)) });
  };
  const waiting = files.filter((f) => !f.artifactId);
  const out: AttachedFile[] = [];
  for (const f of files) {
    let { artifactId, sha256 } = f;
    if (!artifactId) {
      const n = waiting.indexOf(f);
      const which = waiting.length > 1 ? `file ${n + 1} of ${waiting.length}` : `“${f.name}”`;
      progress(`Sending ${which}`);
      const blob = await blobs.get(f.blobKey);
      if (!blob) throw new MissingFile(f.name);
      if (blob.size !== f.size) throw new MissingFile(f.name);
      const go = (transferId?: string) =>
        upload(ctx, {
          name: uploadName(f.name),
          blob,
          destination: { kind: "artifact" },
          label: `Send ${f.name}`,
          ...(transferId ? { transferId } : {}),
          ...(f.sha256 ? { sha256: f.sha256 } : {}),
          onDeclared: (id) => record(f.key, { transferId: id }),
          onProgress: (sent, size) => {
            const fraction = size ? sent / size : 1;
            setProgress(f.key, fraction);
            progress(`Sending ${which} · ${Math.floor(fraction * 100)}%`);
          },
        });
      setProgress(f.key, 0);
      try {
        let r;
        try {
          r = await go(f.transferId);
        } catch (e) {
          // An earlier upload the computer has closed can't be continued: start it again.
          if (!f.transferId || !(e instanceof TransferError) || e.resumable) throw e;
          record(f.key, { transferId: undefined });
          r = await go();
        }
        if (!r.artifactId) throw new TransferError(`The computer didn’t confirm “${f.name}”.`, r.transferId, false);
        artifactId = r.artifactId;
        sha256 = r.sha256;
        record(f.key, { artifactId, sha256, transferId: undefined });
      } finally {
        setProgress(f.key, null);
      }
    }
    out.push({ artifact_id: artifactId, sha256: sha256!, name: f.name, media_type: f.mediaType, size: f.size });
  }
  return out;
}

export class MissingFile extends Error {
  constructor(readonly fileName: string) {
    super(`“${fileName}” is no longer on this device. Remove it and attach it again.`);
  }
}

/** The message's bundle with its files: the markup's, or one of its own. */
export function withFiles(bundle: AnnotationBundle | null, files: AttachedFile[]): AnnotationBundle | null {
  if (!files.length) return bundle;
  if (bundle) return { ...bundle, files };
  return { bundle_id: uuidv7(Date.now()), revision: 1, sources: [], marks: [], comments: [], steps: [], general_note: null, speech: null, files };
}
