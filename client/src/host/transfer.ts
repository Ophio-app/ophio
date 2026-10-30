// SPDX-License-Identifier: Apache-2.0
import { commandResult } from "./commandResult";
//
// File transfers between the phone and the computer. A transfer is declared
// by command, then its bytes move on a separate transfer link. Uploads carry
// the exact size and SHA-256, and the computer verifies the hash before the
// file appears at its destination, so a completed upload is exactly the bytes
// reviewed here. Downloads are verified on the phone the same way before
// anything is offered to the person.

import type { DownloadSource, TransferDestination, TransferServerFrame } from "../protocol/types";
import type { Platform, TransferLink } from "./link";
import type { HostSession } from "./session";

/** Largest chunk either side sends (protocol MAX_CHUNK_BYTES). */
export const CHUNK_BYTES = 256 * 1024;
/** Largest file Android's Downloads take from Save to phone (Downloads.kt). */
export const ANDROID_SAVE_LIMIT = 1024 ** 3;
const gigabytes = (n: number) =>
  `${(n / 1024 ** 3).toLocaleString(undefined, { maximumFractionDigits: 1 })} GB`;

export async function sha256Hex(blob: Blob): Promise<string> {
  const d = new Uint8Array(await crypto.subtle.digest("SHA-256", await blob.arrayBuffer()));
  let hex = "";
  for (const b of d) hex += b.toString(16).padStart(2, "0");
  return hex;
}

export interface UploadOptions {
  name: string;
  blob: Blob;
  destination: TransferDestination;
  evidence?: import("../protocol/types").ArtifactLink;
  /** Command label shown in activity, e.g. "Send screenshot". */
  label: string;
  /** Resume an earlier transfer of the same bytes instead of declaring a new one. */
  transferId?: string;
  /** Persisted id for the declaration, so repeating it after a lost receipt
   * returns the same transfer instead of declaring another. */
  commandId?: string;
  /** Hash already computed for these bytes. */
  sha256?: string;
  signal?: AbortSignal;
  onProgress?(sent: number, size: number): void;
  /** Called once the computer has declared the transfer. */
  onDeclared?(transferId: string): void | Promise<void>;
}

export interface UploadResult {
  transferId: string;
  sha256: string;
  /** Set when the destination was `artifact`. */
  artifactId: string | null;
}

export class TransferError extends Error {
  constructor(
    message: string,
    /** Known once declared; the transfer can be resumed or cancelled by id. */
    readonly transferId: string | null,
    /** The link dropped mid-way; resuming continues from the verified offset. */
    readonly resumable: boolean,
    /** Download bytes received so far, to pass back as `resume.received`. */
    readonly partial: Blob | null = null,
  ) {
    super(message);
  }
}

export { TransferError as UploadError };

type Ctx = { platform: Platform; profileId: string; session: HostSession };

export async function upload(ctx: Ctx, o: UploadOptions): Promise<UploadResult> {
  const sha256 = o.sha256 ?? (await sha256Hex(o.blob));
  let transferId = o.transferId ?? null;
  if (!transferId) {
    const r = await ctx.session.command(
      { command: "create_upload", name: o.name, size: o.blob.size, sha256, destination: o.destination, evidence: o.evidence },
      { label: o.label, ...(o.commandId ? { commandId: o.commandId } : {}) },
    );
    const result = await commandResult(ctx.session, r);
    if (result?.kind !== "transfer") throw new TransferError("The computer didn’t start the transfer.", null, false);
    transferId = result.transfer.transfer_id;
  }
  await o.onDeclared?.(transferId);
  const link = ctx.platform.transfer(ctx.profileId);
  try {
    return await send(link, transferId, sha256, o);
  } finally {
    link.close();
  }
}

async function send(link: TransferLink, id: string, sha256: string, o: UploadOptions): Promise<UploadResult> {
  const frames = new FrameQueue(link, id);
  try {
    await frames.online(o.signal);
    if (!link.send({ type: "upload", transfer_id: id })) throw new TransferError("Not connected.", id, true);
    const start = await frames.next(o.signal);
    if (start.type === "failed") throw new TransferError(start.reason, id, false);
    if (start.type !== "resume_at") throw new TransferError("Unexpected reply from the computer.", id, true);
    const size = o.blob.size;
    for (let at = Number(start.offset); at < size; ) {
      if (o.signal?.aborted) {
        link.send({ type: "cancel", transfer_id: id });
        throw new TransferError("Cancelled.", id, false);
      }
      frames.throwIfFailed();
      const end = Math.min(size, at + CHUNK_BYTES);
      const bytes = new Uint8Array(await o.blob.slice(at, end).arrayBuffer());
      if (!(await link.chunk(bytes))) throw new TransferError("The connection dropped. It can continue where it stopped.", id, true);
      at = end;
      o.onProgress?.(at, size);
    }
    if (!link.send({ type: "finish", transfer_id: id })) throw new TransferError("Not connected.", id, true);
    const done = await frames.next(o.signal);
    if (done.type === "failed") throw new TransferError(done.reason, id, false);
    if (done.type !== "completed") throw new TransferError("Unexpected reply from the computer.", id, true);
    if (done.sha256 !== sha256) throw new TransferError("The computer received different bytes.", id, false);
    return { transferId: id, sha256, artifactId: done.artifact_id };
  } finally {
    frames.dispose();
  }
}

export interface DownloadOptions {
  shownInTask?: boolean;
  source: DownloadSource;
  /** Command label shown in activity, e.g. "Send to this device". */
  label: string;
  /** Continue an earlier download: its id and the bytes already received. */
  resume?: { transferId: string; received: Blob };
  signal?: AbortSignal;
  onProgress?(received: number, size: number): void;
  onDeclared?(transferId: string, size: number): void;
}

export interface DownloadResult {
  transferId: string;
  sha256: string;
  blob: Blob;
}

export async function download(ctx: Ctx, o: DownloadOptions): Promise<DownloadResult> {
  let transferId = o.resume?.transferId ?? null;
  if (!transferId) {
    const r = await ctx.session.command({ command: "create_download", source: o.source, ...(o.shownInTask ? { shown_in_task: true } : {}) }, { label: o.label });
    const result = await commandResult(ctx.session, r);
    if (result?.kind !== "transfer") throw new TransferError("The computer didn’t start the transfer.", null, false);
    const { transfer } = result;
    transferId = transfer.transfer_id;
    // Nothing has moved yet. A file this device can't keep isn't started.
    const limit = ctx.platform.downloadLimit;
    if (limit !== undefined && transfer.size > limit) {
      void ctx.session
        .command({ command: "cancel_transfer", transfer_id: transferId }, { label: `Cancel ${transfer.name}` })
        .catch(() => undefined);
      throw new TransferError(
        `“${transfer.name}” is ${gigabytes(transfer.size)}. This phone takes files up to ${gigabytes(limit)}, so fetch this one another way, such as on the computer itself.`,
        transferId,
        false,
      );
    }
    o.onDeclared?.(transferId, transfer.size);
  }
  const link = ctx.platform.transfer(ctx.profileId);
  try {
    return await receive(link, transferId, o);
  } finally {
    link.close();
  }
}

async function receive(link: TransferLink, id: string, o: DownloadOptions): Promise<DownloadResult> {
  let parts: BlobPart[] = o.resume ? [o.resume.received] : [];
  const requestedOffset = o.resume?.received.size ?? 0;
  let received = requestedOffset;
  let size = 0;
  let initialized = false;
  const buffered: Uint8Array[] = [];
  const append = (bytes: Uint8Array) => {
    parts.push(bytes as Uint8Array<ArrayBuffer>);
    received += bytes.byteLength;
    o.onProgress?.(received, size);
  };
  const frames = new FrameQueue(link, id, (bytes) => {
    // Native channels can deliver the header and chunks in one callback,
    // before the await below resumes. Apply its offset before keeping bytes.
    if (initialized) append(bytes);
    else buffered.push(bytes);
  });
  const fail = (message: string, resumable: boolean) =>
    new TransferError(message, id, resumable, resumable ? new Blob(parts) : null);
  try {
    await frames.online(o.signal).catch((e: unknown) => {
      throw e instanceof TransferError ? fail(e.message, e.resumable) : e;
    });
    if (!link.send({ type: "download", transfer_id: id, offset: requestedOffset })) throw fail("Not connected.", true);
    const start = await frames.next(o.signal);
    if (start.type === "failed") throw fail(start.reason, false);
    if (start.type !== "sending") throw fail("Unexpected reply from the computer.", true);
    const offset = Number(start.offset);
    if (offset !== requestedOffset) {
      // The computer can only continue from where it says; anything else
      // would splice different bytes together. Start again from zero.
      if (offset !== 0) throw fail("The computer couldn’t continue this download.", false);
      parts = [];
      received = 0;
    }
    size = Number(start.size);
    o.onProgress?.(received, size);
    initialized = true;
    for (const bytes of buffered) append(bytes);
    buffered.length = 0;
    let done: TransferServerFrame;
    try {
      done = await frames.next(o.signal);
    } catch (e) {
      if (o.signal?.aborted) link.send({ type: "cancel", transfer_id: id });
      throw e instanceof TransferError ? fail(e.message, e.resumable) : e;
    }
    if (done.type === "failed") throw fail(done.reason, false);
    if (done.type !== "completed") throw fail("Unexpected reply from the computer.", true);
    const blob = new Blob(parts);
    // The chunks are in the Blob now; let them go before hashing reads it.
    parts = [];
    if (blob.size !== size) throw fail(`Received ${blob.size} of ${size} bytes.`, false);
    const sha256 = await sha256Hex(blob);
    if (sha256 !== start.sha256 || sha256 !== done.sha256) throw fail("The file changed or was damaged on the way.", false);
    return { transferId: id, sha256, blob };
  } finally {
    frames.dispose();
  }
}

/** Server frames for one transfer, in order, plus link-state tracking. */
class FrameQueue {
  #queue: TransferServerFrame[] = [];
  #waiters: ((f: TransferServerFrame | Error) => void)[] = [];
  #online = false;
  #onlineWaiters: ((e: Error | null) => void)[] = [];
  #failed: string | null = null;
  #connectionError: TransferError | null = null;
  #stop: () => void;
  /** Last time anything arrived; waits time out on silence, not on total duration. */
  #heard = Date.now();

  constructor(
    link: TransferLink,
    readonly id: string,
    onChunk?: (bytes: Uint8Array) => void,
  ) {
    this.#stop = link.listen((e) => {
      this.#heard = Date.now();
      if (e.type === "chunk") {
        onChunk?.(e.data);
      } else if (e.type === "state") {
        const s = e.state.state;
        if (s === "online") {
          this.#online = true;
          this.#connectionError = null;
          this.#onlineWaiters.splice(0).forEach((w) => w(null));
        } else if (s === "offline" || s === "failed") {
          this.#online = false;
          const state = e.state;
          if (state.state !== "offline" && state.state !== "failed") return;
          const missing = state.state === "offline" && state.code === "not_found";
          const reason = state.state === "offline" ? state.reason : state.detail;
          const said = /[.!?…]$/.test(reason.trim()) ? reason.trim() : `${reason.trim()}.`;
          const err = new TransferError(`${said} ${missing ? "Start a new transfer." : "Resolve this, then retry to continue where it stopped."}`, id, !missing);
          this.#connectionError = err;
          this.#onlineWaiters.splice(0).forEach((w) => w(err));
          this.#waiters.splice(0).forEach((w) => w(err));
        }
      } else if (e.type === "text" && e.frame.transfer_id === id) {
        if (e.frame.type === "failed") this.#failed = e.frame.reason;
        const w = this.#waiters.shift();
        if (w) w(e.frame);
        else this.#queue.push(e.frame);
      }
    });
  }

  online(signal?: AbortSignal): Promise<void> {
    if (this.#connectionError) return Promise.reject(this.#connectionError);
    if (this.#online) return Promise.resolve();
    return withTimeout(
      new Promise<void>((resolve, reject) => this.#onlineWaiters.push((e) => (e ? reject(e) : resolve()))),
      15_000,
      () => this.#heard,
      signal,
      this.id,
    );
  }

  next(signal?: AbortSignal): Promise<TransferServerFrame> {
    if (this.#connectionError) return Promise.reject(this.#connectionError);
    const f = this.#queue.shift();
    if (f) return Promise.resolve(f);
    return withTimeout(
      new Promise<TransferServerFrame>((resolve, reject) =>
        this.#waiters.push((x) => (x instanceof Error ? reject(x) : resolve(x))),
      ),
      60_000,
      () => this.#heard,
      signal,
      this.id,
    );
  }

  throwIfFailed(): void {
    if (this.#connectionError) throw this.#connectionError;
    if (this.#failed) throw new TransferError(this.#failed, this.id, false);
  }

  dispose(): void {
    this.#stop();
  }
}

/** Reject when nothing has been heard for `ms`, or on abort. */
function withTimeout<T>(p: Promise<T>, ms: number, heard: () => number, signal: AbortSignal | undefined, id: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const check = () => {
      const quiet = Date.now() - heard();
      if (quiet >= ms) reject(new TransferError("The computer stopped answering.", id, true));
      else t = setTimeout(check, ms - quiet);
    };
    let t = setTimeout(check, ms);
    const abort = () => reject(new TransferError("Cancelled.", id, false));
    if (signal?.aborted) abort();
    signal?.addEventListener("abort", abort, { once: true });
    p.then(resolve, reject).finally(() => {
      clearTimeout(t);
      signal?.removeEventListener("abort", abort);
    });
  });
}
