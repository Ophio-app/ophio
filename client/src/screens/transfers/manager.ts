// SPDX-License-Identifier: Apache-2.0
import { commandResult } from "../../host/commandResult";
import type { App } from "../../app/context";
import { describeError } from "../../app/context";
import type { Platform, TransferEvent, TransferLink } from "../../host/link";
import type { HostSession } from "../../host/session";
import {
  download,
  upload,
  TransferError,
  sha256Hex,
} from "../../host/transfer";
import type {
  DownloadSource,
  TransferDestination,
  TransferRecord,
  UploadCollision,
} from "../../protocol/types";
import { Store } from "../../state/store";
import { readSavedJson, UnpreservedJson } from "../../state/savedJson";
import { uuidv7 } from "../../protocol/ids";
import { announce } from "../../design/Announcer";
import { transferBytes } from "./storage";

export type JobPhase =
  | "preparing"
  | "sending"
  | "verifying"
  | "paused"
  | "completed"
  | "failed"
  | "unknown"
  | "cancelled";
export interface TransferJob {
  id: string;
  name: string;
  direction: "upload" | "download";
  size: number;
  transferred: number;
  phase: JobPhase;
  createdAt: number;
  updatedAt: number;
  transferId: string | null;
  sha256: string | null;
  destination: TransferDestination | null;
  source: DownloadSource | null;
  artifactId: string | null;
  hostPath: string | null;
  error: string | null;
  storageError: string | null;
  resumable: boolean;
  /** Where the phone last saved this verified download; absent until saved. */
  savedAs?: string | null;
  collision?: UploadCollision;
  /** Command id of the upload declaration, kept so repeating it is deduplicated. */
  declaration?: string;
  /** An upload paused because its connection dropped, not by the person. It
   * continues by itself while the app is in the foreground with a working
   * link, until it finishes, fails or the person pauses or cancels it. */
  interrupted?: boolean;
  /** The person started it over as a new transfer, so it reads as history. */
  retried?: boolean;
}
interface ManagerState {
  jobs: Record<string, TransferJob>;
  storageError: string | null;
  storageRecovery: string | null;
}
interface Runner {
  paused: boolean;
  close?: () => void;
  promise: Promise<void>;
}
/** The saved jobs. Throws when history can neither be read nor preserved in a
 * recovery copy, which is not the same as there being none. */
function load(key: string): { jobs: Record<string, TransferJob>; recovered: boolean } {
  const jobs: Record<string, TransferJob> = {};
  const { value: raw, recovered } = readSavedJson(localStorage, key);
  if (raw && typeof raw === "object")
    for (const [id, value] of Object.entries(raw)) {
      if (
        !value ||
        typeof value !== "object" ||
        typeof value.id !== "string" ||
        value.id !== id ||
        typeof value.name !== "string" ||
        typeof value.size !== "number" ||
        !Number.isFinite(value.createdAt) ||
        !Number.isFinite(value.updatedAt) ||
        !Number.isFinite(value.transferred) ||
        ![
          "preparing",
          "sending",
          "verifying",
          "paused",
          "completed",
          "failed",
          "unknown",
          "cancelled",
        ].includes(value.phase) ||
        (value.transferId !== null &&
          typeof value.transferId !== "string") ||
        (value.sha256 !== null && typeof value.sha256 !== "string") ||
        !["upload", "download"].includes(value.direction)
      )
        continue;
      const j = value as TransferJob;
      if (j.savedAs !== undefined && typeof j.savedAs !== "string")
        j.savedAs = null;
      if (j.retried !== undefined && j.retried !== true) delete j.retried;
      if (j.declaration !== undefined && typeof j.declaration !== "string")
        delete j.declaration;
      // After the app closed, continuing is the person's choice again.
      delete j.interrupted;
      const continuable = !!j.transferId || !!j.declaration;
      jobs[id] = ["preparing", "sending", "verifying"].includes(j.phase)
        ? {
            ...j,
            phase: continuable ? "paused" : "unknown",
            error: continuable
              ? "The app closed. Continue when ready."
              : "The computer hasn’t confirmed this transfer. Check its history before sending again.",
            resumable: continuable,
          }
        : j;
    }
  return { jobs, recovered };
}
/** Shown while the saved history is unread. Saving would replace it, so
 * nothing is saved until it has been read or preserved for recovery. */
const UNREAD =
  "Transfer history on this device could not be read, so earlier transfers aren’t listed. It was left untouched and is read again before anything is saved. Keep the app open until transfers finish.";
const RECOVERED =
  "Transfer history on this device was damaged. A copy has been kept for recovery, but earlier transfers could not be listed. New transfer records can be saved again.";
function loadError(error: unknown): string {
  return error instanceof UnpreservedJson
    ? "Transfer history on this device is damaged and could not be set aside. The original is untouched. New transfer records are held only until the app closes. Free some device storage; saving will try again."
    : UNREAD;
}
/** First wait before an upload interrupted in the foreground tries again. The
 * computer lets go of a connection only after it has been silent a while, so
 * the waits double up to RESUME_MAX_MS rather than asking it again and again. */
const RESUME_FIRST_MS = 2_000;
const RESUME_MAX_MS = 60_000;
/** Byte counters are advisory; recovery uses the declaration and the computer's
 * confirmed offset. Publish and save progress at most four times a second. */
const PROGRESS_MS = 250;
type Progress = Pick<TransferJob, "transferred" | "size" | "phase" | "updatedAt">;
/** Owns transfers beyond any screen's lifetime. Resuming is an explicit action,
 * except for an upload its connection interrupted. */
export class TransferManager extends Store<ManagerState> {
  private runners = new Map<string, Runner>();
  private memory = new Map<string, Blob>();
  private key: string;
  /** The saved history couldn't be read and has not been read since. */
  private unread: boolean;
  private watching: { session: HostSession; stop: () => void } | null = null;
  private retry: ReturnType<typeof setTimeout> | undefined;
  private wait = RESUME_FIRST_MS;
  private pendingProgress = new Map<string, Progress>();
  private progressTimer: ReturnType<typeof setTimeout> | undefined;
  constructor(private app: App) {
    const key = `transfers.v1.${app.profile.profileId}`;
    let jobs: Record<string, TransferJob> = {};
    let storageError: string | null = null, storageRecovery: string | null = null;
    try {
      const loaded = load(key);
      jobs = loaded.jobs;
      if (loaded.recovered) storageRecovery = RECOVERED;
    } catch (error) {
      storageError = loadError(error);
    }
    super({ jobs, storageError, storageRecovery });
    this.key = key;
    this.unread = storageError !== null;
    this.watch();
  }
  bind(app: App) {
    this.app = app;
    this.watch();
  }
  /** Interrupted uploads continue when the app returns to the foreground or
   * its link comes back, whichever is last. */
  private watch() {
    if (this.watching?.session === this.app.session) return;
    this.watching?.stop();
    const resume = () => {
      // Each return to the foreground or new connection starts the waits over.
      this.wait = RESUME_FIRST_MS;
      this.resumeInterrupted();
    };
    // Coming back is also a chance to read history that couldn't be read.
    const back = () => {
      if (this.unread) this.persist(this.takeProgress());
      resume();
    };
    const visibility = () => {
      if (document.visibilityState === "hidden") this.flushProgress();
      back();
    };
    const flush = () => this.flushProgress();
    const offLink = this.app.session.link.subscribe(back);
    const offForeground = this.app.platform.onForeground?.(back);
    document.addEventListener("visibilitychange", visibility);
    window.addEventListener("pagehide", flush);
    this.watching = {
      session: this.app.session,
      stop: () => {
        offLink();
        offForeground?.();
        document.removeEventListener("visibilitychange", visibility);
        window.removeEventListener("pagehide", flush);
      },
    };
    resume();
  }
  private canResume() {
    return document.visibilityState !== "hidden" && this.app.session.online;
  }
  private resumeInterrupted() {
    clearTimeout(this.retry);
    this.retry = undefined;
    if (!this.canResume()) return;
    for (const job of Object.values(this.get().jobs))
      if (job.interrupted && job.phase === "paused" && job.resumable)
        this.run(job.id);
  }
  /** Nothing else brings back an upload interrupted while the app is in the
   * foreground with a working link, so it tries again after a wait. */
  private resumeLater() {
    if (this.retry !== undefined) return;
    this.retry = setTimeout(() => this.resumeInterrupted(), this.wait);
    this.wait = Math.min(this.wait * 2, RESUME_MAX_MS);
  }
  /** A computer-confirmed upload can finish even if its side-link receipt was lost. */
  observe(records: readonly TransferRecord[]) {
    for (const job of Object.values(this.get().jobs)) {
      if (job.direction !== "upload" || !job.transferId) continue;
      const record = records.find((r) => r.transfer_id === job.transferId);
      // Publication may or may not have happened. It cannot be resumed, and
      // the person checks the computer before clearing it.
      if (record?.state.state === "reconciliation_required") {
        if (job.resumable || job.error)
          this.patch(job.id, { resumable: false, error: null });
        continue;
      }
      if (!record || record.state.state !== "completed") continue;
      if (job.sha256 && record.sha256 !== job.sha256) continue;
      if (job.phase === "completed" && job.hostPath === record.host_path)
        continue;
      this.patch(job.id, {
        phase: "completed",
        hostPath: record.host_path,
        sha256: record.sha256,
        transferred: record.size,
        size: record.size,
        resumable: false,
        error: null,
      });
    }
  }
  private progress(id: string, transferred: number, size: number) {
    const job = this.get().jobs[id];
    // A host completion can arrive before the side link's last callback.
    if (!job || !["preparing", "sending", "verifying"].includes(job.phase)) return;
    this.pendingProgress.set(id, {
      transferred, size,
      phase: transferred === size ? "verifying" : "sending",
      updatedAt: Date.now(),
    });
    // Keep one deadline across all active jobs; continuous chunks must not
    // postpone publishing indefinitely or copy the saved history per chunk.
    if (this.progressTimer === undefined)
      this.progressTimer = setTimeout(() => this.flushProgress(), PROGRESS_MS);
  }
  private takeProgress(): Record<string, TransferJob> {
    clearTimeout(this.progressTimer);
    this.progressTimer = undefined;
    if (!this.pendingProgress.size) return this.get().jobs;
    const jobs = { ...this.get().jobs };
    for (const [id, progress] of this.pendingProgress) {
      const job = jobs[id];
      if (job) jobs[id] = { ...job, ...progress };
    }
    this.pendingProgress.clear();
    return jobs;
  }
  private flushProgress() {
    if (this.pendingProgress.size) this.persist(this.takeProgress());
  }
  private patch(id: string, patch: Partial<TransferJob>) {
    if (!this.get().jobs[id]) return;
    // Durable transitions include the latest counters, then override their
    // phase. No delayed progress can turn a completed/paused job into sending.
    const jobs = this.takeProgress();
    const current = jobs[id];
    if (!current) return;
    this.persist({
      ...jobs,
      [id]: { ...current, ...patch, updatedAt: Date.now() },
    });
  }
  private persist(jobs: Record<string, TransferJob>) {
    let storageRecovery = this.get().storageRecovery;
    if (this.unread)
      try {
        // Saved jobs hold the ids that find downloaded bytes on this device,
        // so they are kept beside the jobs added since.
        const loaded = load(this.key);
        jobs = { ...loaded.jobs, ...jobs };
        if (loaded.recovered) storageRecovery = RECOVERED;
        this.unread = false;
      } catch (error) {
        this.set({ jobs, storageError: loadError(error), storageRecovery });
        return;
      }
    let storageError: string | null = null;
    try {
      localStorage.setItem(this.key, JSON.stringify(jobs));
    } catch {
      storageError =
        "Transfer history could not be saved on this device. Keep the app open until transfers finish.";
    }
    this.set({ jobs, storageError, storageRecovery });
  }
  private blobKey(id: string) {
    return `${this.app.profile.profileId}:${id}`;
  }
  private async put(id: string, blob: Blob) {
    this.memory.set(id, blob);
    try {
      await transferBytes.put(this.blobKey(id), blob);
      this.patch(id, { storageError: null });
    } catch (e) {
      this.patch(id, {
        storageError:
          describeError(e, this.app.profile.name) +
          " Keep the app open to retain these bytes.",
      });
    }
  }
  /** Records where Save to phone put a download, so its row can say so. */
  saved(id: string, savedAs: string) {
    this.patch(id, { savedAs });
  }
  /** Marks a failed or cancelled transfer as started over by a new one. */
  retried(id: string) {
    this.patch(id, { retried: true });
  }
  async blob(id: string): Promise<Blob | null> {
    return this.memory.get(id) ?? (await transferBytes.get(this.blobKey(id)));
  }
  private create(
    name: string,
    direction: "upload" | "download",
    size: number,
    destination: TransferDestination | null,
    source: DownloadSource | null,
  ): string {
    const now = Date.now(),
      id = uuidv7(now);
    const job: TransferJob = {
      id,
      name,
      direction,
      size,
      destination,
      source,
      createdAt: now,
      updatedAt: now,
      transferred: 0,
      phase: "preparing",
      transferId: null,
      sha256: null,
      artifactId: null,
      hostPath: null,
      error: null,
      storageError: null,
      resumable: false,
    };
    this.persist({ ...this.takeProgress(), [id]: job });
    return id;
  }
  startUpload(
    name: string,
    blob: Blob,
    destination: TransferDestination,
    collision?: UploadCollision,
  ): string {
    const id = this.create(name, "upload", blob.size, destination, null);
    if (collision) this.patch(id, { collision });
    this.run(id, this.put(id, blob));
    return id;
  }
  startDownload(source: DownloadSource, name: string): string {
    const id = this.create(name, "download", 0, null, source);
    this.run(id);
    return id;
  }
  pause(id: string) {
    const runner = this.runners.get(id);
    if (!runner) return;
    runner.paused = true;
    this.flushProgress();
    runner.close?.();
  }
  continue(id: string) {
    const j = this.get().jobs[id];
    if (j?.resumable && j.phase === "paused" && this.app.session.online)
      this.run(id);
  }
  private run(id: string, prepared: Promise<void> = Promise.resolve()) {
    if (this.runners.has(id)) return;
    const runner: Runner = { paused: false, promise: Promise.resolve() };
    this.runners.set(id, runner);
    runner.promise = prepared
      .then(() => this.execute(id, runner))
      .finally(() => this.runners.delete(id));
  }
  /** Closing this side link pauses byte transfer; it never sends a cancellation frame. */
  private pausablePlatform(runner: Runner): Platform {
    return {
      ...this.app.platform,
      transfer: (profileId) => {
        const real = this.app.platform.transfer(profileId);
        const listeners = new Set<(e: TransferEvent) => void>();
        let stopped = false;
        const pause = () => {
          if (stopped) return;
          stopped = true;
          for (const l of listeners)
            l({
              type: "state",
              state: {
                state: "offline",
                since: Date.now(),
                reason: "Paused on this device",
                retryAt: null,
              },
            });
          real.close();
        };
        runner.close = pause;
        const link: TransferLink = {
          send: (f) => !stopped && real.send(f),
          chunk: (b) => (stopped ? Promise.resolve(false) : real.chunk(b)),
          close: () => {
            stopped = true;
            real.close();
          },
          listen: (l) => {
            listeners.add(l);
            const stop = real.listen((e) => {
              if (!stopped) l(e);
            });
            if (runner.paused) queueMicrotask(pause);
            return () => {
              listeners.delete(l);
              stop();
            };
          },
        };
        return link;
      },
    };
  }
  private async execute(id: string, runner: Runner) {
    const job = this.get().jobs[id];
    if (!job) return;
    this.patch(id, { phase: "preparing", error: null });
    const ctx = {
      session: this.app.session,
      profileId: this.app.profile.profileId,
      platform: this.pausablePlatform(runner),
    };
    try {
      if (!this.app.session.online) {
        this.patch(id, {
          phase: "paused",
          resumable: true,
          interrupted: job.direction === "upload" && !runner.paused,
          error:
            job.direction === "upload"
              ? "Not connected to the computer."
              : "Connect to the computer, then continue.",
        });
        return;
      }
      const onProgress = (transferred: number, size: number) =>
        this.progress(id, transferred, size);
      if (job.direction === "upload") {
        const blob = await this.blob(id);
        if (!blob || !job.destination)
          throw new Error(
            "The original bytes are missing from this device. Select the file again to start a new transfer.",
          );
        const hash = job.sha256 ?? (await sha256Hex(blob));
        this.patch(id, { sha256: hash });
        if (runner.paused) {
          this.patch(id, {
            phase: "paused",
            resumable: true,
            interrupted: false,
            error: null,
          });
          return;
        }
        let transferId = job.transferId;
        // Persist the declaration's id before sending it: after a lost
        // receipt, the same id returns the same transfer, never a second one.
        const commandId = transferId
          ? undefined
          : (job.declaration ?? uuidv7(Date.now()));
        if (commandId !== job.declaration) this.patch(id, { declaration: commandId });
        if (!transferId && job.collision) {
          const receipt = await ctx.session.command(
            {
              command: "create_upload_reviewed",
              name: job.name,
              size: blob.size,
              sha256: hash,
              destination: job.destination,
              collision: job.collision,
            },
            { label: `Send ${job.name}`, commandId },
          );
          const declaration = await commandResult(ctx.session, receipt);
          if (declaration?.kind !== "transfer")
            throw new Error(
              "The computer hasn’t confirmed the transfer declaration.",
            );
          transferId = declaration.transfer.transfer_id;
          this.patch(id, { transferId, name: declaration.transfer.name });
        }
        const result = await upload(ctx, {
          name: job.name,
          blob,
          destination: job.destination,
          sha256: hash,
          label: `Send ${job.name}`,
          ...(transferId ? { transferId } : { commandId }),
          onProgress,
          onDeclared: (transferId) =>
            this.patch(id, { transferId, phase: "sending" }),
        });
        this.patch(id, {
          phase: "completed",
          sha256: result.sha256,
          transferId: result.transferId,
          artifactId: result.artifactId,
          transferred: blob.size,
          resumable: false,
          interrupted: false,
        });
        this.wait = RESUME_FIRST_MS;
      } else {
        if (!job.source)
          throw new Error(
            "The file source is missing. Open it in Library to download it again.",
          );
        const partial = job.transferId ? await this.blob(id) : null;
        const result = await download(ctx, {
          source: job.source,
          label: `Receive ${job.name}`,
          ...(job.transferId
            ? {
                resume: {
                  transferId: job.transferId,
                  received: partial ?? new Blob(),
                },
              }
            : {}),
          onProgress,
          onDeclared: (transferId, size) =>
            this.patch(id, { transferId, size, phase: "sending" }),
        });
        await this.put(id, result.blob);
        this.patch(id, {
          phase: "completed",
          sha256: result.sha256,
          transferId: result.transferId,
          transferred: result.blob.size,
          size: result.blob.size,
          resumable: false,
        });
      }
      announce(`${job.name} arrived and its bytes were verified.`);
    } catch (e) {
      if (e instanceof TransferError) {
        if (job.direction === "download" && e.partial)
          await this.put(id, e.partial);
        this.patch(id, {
          phase: e.resumable ? "paused" : "failed",
          transferId: e.transferId,
          resumable: e.resumable,
          interrupted: job.direction === "upload" && e.resumable && !runner.paused,
          error: runner.paused ? null : e.message,
        });
      } else {
        const now = this.get().jobs[id];
        // Repeating a persisted declaration cannot declare a second transfer.
        const redeclare = !!now?.declaration && !now.transferId;
        this.patch(id, {
          phase: !this.app.session.online ? (redeclare ? "paused" : "unknown") : "failed",
          resumable: !this.app.session.online && redeclare,
          interrupted: !this.app.session.online && redeclare && !runner.paused,
          error: !this.app.session.online
            ? redeclare
              ? "The computer hasn’t confirmed this transfer. Continue checks it without sending it twice."
              : "The computer hasn’t confirmed this transfer. Check history before sending again."
            : describeError(e, this.app.profile.name),
        });
      }
      const interrupted = this.get().jobs[id]?.interrupted;
      if (interrupted && this.canResume()) this.resumeLater();
      // An upload that was already waiting to continue says so only once.
      if (interrupted && job.interrupted) return;
      announce(
        `${job.name}: ${this.get().jobs[id]?.phase === "paused" ? "paused" : "needs attention"}.`,
      );
    }
  }
  async cancel(id: string): Promise<void> {
    // A cancelled upload never continues by itself, even while this runs.
    this.patch(id, { interrupted: false });
    const runner = this.runners.get(id);
    if (runner) {
      runner.paused = true;
      runner.close?.();
      await runner.promise;
    }
    this.observe(Object.values(this.app.session.state.get().transfers));
    const job = this.get().jobs[id];
    if (!job) return;
    const checked =
      !!job.transferId &&
      this.app.session.state.get().transfers[job.transferId]?.state.state ===
        "reconciliation_required";
    if (job.phase === "completed")
      throw new Error(
        "This transfer already finished. Its file was kept. Remove it on the computer if needed.",
      );
    if (job.transferId) {
      const receipt = await this.app.session.command(
        { command: "cancel_transfer", transfer_id: job.transferId },
        { label: `Cancel ${job.name}` },
      );
      await commandResult(this.app.session, receipt);
    } else if (job.phase === "unknown")
      throw new Error(
        "The computer hasn’t confirmed whether this transfer was created. Check its history first.",
      );
    this.observe(Object.values(this.app.session.state.get().transfers));
    if (this.get().jobs[id]?.phase === "completed")
      throw new Error(
        "The file finished arriving before cancellation. It remains on the computer.",
      );
    this.patch(id, { phase: "cancelled", resumable: false, error: null });
    announce(checked ? `Cleared ${job.name}.` : `Cancelled ${job.name}.`);
  }
}
const managers = new Map<string, TransferManager>();
export function transferManager(app: App): TransferManager {
  let m = managers.get(app.profile.profileId);
  if (!m) {
    m = new TransferManager(app);
    managers.set(app.profile.profileId, m);
  }
  m.bind(app);
  return m;
}
