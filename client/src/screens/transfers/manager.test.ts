// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { App } from "../../app/context";
import type { Platform, TransferLink } from "../../host/link";
import { HostSession } from "../../host/session";
import { TransferError, upload, download } from "../../host/transfer";
import { TransferManager, type TransferJob } from "./manager";
import { transferBytes } from "./storage";
import type { TransferRecord } from "../../protocol/types";
vi.mock("./storage", () => ({
  transferBytes: {
    put: vi.fn().mockResolvedValue(undefined),
    get: vi.fn().mockResolvedValue(null),
  },
}));
vi.mock("../../host/transfer", async (original) => {
  const actual = await original<typeof import("../../host/transfer")>();
  return {
    ...actual,
    upload: vi.fn(),
    download: vi.fn(),
    sha256Hex: vi.fn().mockResolvedValue("expected-sha"),
  };
});
// The diagnostics trail saves on a timer of its own; these tests count transfer writes.
vi.mock("../../diagnostics/trail", async (original) => {
  const actual = await original<typeof import("../../diagnostics/trail")>();
  return { ...actual, trail: new actual.PhoneTrail(Date.now, null) };
});
function setup() {
  const session = new HostSession("p", {
    connect() {},
    disconnect() {},
    listen: () => () => {},
    send: () => true,
  });
  session.link.set({ state: "online", since: 0 });
  const close = vi.fn(),
    send = vi.fn(() => true);
  const link: TransferLink = {
    send,
    close,
    chunk: async () => true,
    listen: () => () => {},
  };
  const platform = { transfer: () => link } as unknown as Platform;
  const app = {
    profile: { profileId: "p", name: "Computer" },
    platform,
    session,
  } as App;
  return { manager: new TransferManager(app), close, send, session };
}
beforeEach(() => {
  localStorage.clear();
  vi.clearAllMocks();
});
describe("transfer lifetime and pause", () => {
  it("pauses the side link without sending cancel and resumes the same id", async () => {
    const { manager, close, send } = setup();
    vi.mocked(upload)
      .mockImplementationOnce(async (ctx, o) => {
        o.onDeclared?.("t1");
        o.onProgress?.(100, 300);
        const link = ctx.platform.transfer("p");
        return new Promise((_resolve, reject) => {
          link.listen((e) => {
            if (e.type === "state" && e.state.state === "offline")
              reject(new TransferError("Paused", "t1", true));
          });
        });
      })
      .mockResolvedValueOnce({
        transferId: "t1",
        sha256: "expected-sha",
        artifactId: null,
      });
    const id = manager.startUpload("notes.md", new Blob(["draft"]), {
      kind: "inbox",
    });
    await vi.waitFor(() =>
      expect(manager.get().jobs[id]?.phase).toBe("sending"),
    );
    manager.pause(id);
    await vi.waitFor(() =>
      expect(manager.get().jobs[id]?.phase).toBe("paused"),
    );
    expect(close).toHaveBeenCalled();
    expect(send).not.toHaveBeenCalled();
    manager.continue(id);
    await vi.waitFor(() =>
      expect(manager.get().jobs[id]?.phase).toBe("completed"),
    );
    expect(vi.mocked(upload).mock.calls[1]?.[1].transferId).toBe("t1");
  });
  it("stores partial downloads and passes those bytes into resume", async () => {
    const { manager } = setup(),
      partial = new Blob(["part"]);
    vi.mocked(download)
      .mockRejectedValueOnce(new TransferError("Dropped", "t2", true, partial))
      .mockResolvedValueOnce({
        transferId: "t2",
        sha256: "verified",
        blob: new Blob(["part complete"]),
      });
    const id = manager.startDownload(
      { kind: "library_file", root_id: "r", path: "notes.md" },
      "notes.md",
    );
    await vi.waitFor(() =>
      expect(manager.get().jobs[id]?.phase).toBe("paused"),
    );
    expect(transferBytes.put).toHaveBeenCalledWith(`p:${id}`, partial);
    manager.continue(id);
    await vi.waitFor(() =>
      expect(manager.get().jobs[id]?.phase).toBe("completed"),
    );
    expect(vi.mocked(download).mock.calls[1]?.[1].resume).toEqual({
      transferId: "t2",
      received: partial,
    });
  });
  it("does not retry or offer corrupt bytes after verification fails", async () => {
    const { manager } = setup();
    vi.mocked(download).mockRejectedValueOnce(
      new TransferError("Hash mismatch", "t3", false),
    );
    const id = manager.startDownload(
      { kind: "artifact", artifact_id: "a" },
      "image.png",
    );
    await vi.waitFor(() =>
      expect(manager.get().jobs[id]?.phase).toBe("failed"),
    );
    expect(manager.get().jobs[id]?.resumable).toBe(false);
    expect(await manager.blob(id)).toBeNull();
    manager.continue(id);
    expect(download).toHaveBeenCalledTimes(1);
  });
  it("never labels a completed finalization as cancelled", async () => {
    const { manager } = setup();
    vi.mocked(upload).mockResolvedValueOnce({
      transferId: "t4",
      sha256: "verified",
      artifactId: null,
    });
    const id = manager.startUpload("n.md", new Blob(["notes"]), {
      kind: "inbox",
    });
    await vi.waitFor(() =>
      expect(manager.get().jobs[id]?.phase).toBe("completed"),
    );
    await expect(manager.cancel(id)).rejects.toThrow("already finished");
    expect(manager.get().jobs[id]?.phase).toBe("completed");
  });
  it("reconciles a lost upload receipt before cancel and retains its destination", async () => {
    const { manager, session } = setup();
    vi.mocked(upload).mockRejectedValueOnce(
      new TransferError("Dropped", "t5", true),
    );
    const id = manager.startUpload("n.md", new Blob(["notes"]), {
      kind: "inbox",
    });
    await vi.waitFor(() =>
      expect(manager.get().jobs[id]?.phase).toBe("paused"),
    );
    const record: TransferRecord = {
      transfer_id: "t5",
      direction: "phone_to_host",
      name: "n.md",
      size: 5,
      sha256: "expected-sha",
      transferred: 5,
      state: { state: "completed" },
      destination: { kind: "inbox" },
      host_path: "/Inbox/n.md",
      created_at: 0,
      updated_at: 1,
    };
    session.state.set({ ...session.state.get(), transfers: { t5: record } });
    await expect(manager.cancel(id)).rejects.toThrow("already finished");
    expect(manager.get().jobs[id]?.phase).toBe("completed");
    const restored = setup().manager.get().jobs[id];
    expect(restored?.phase).toBe("completed");
    expect(restored?.hostPath).toBe("/Inbox/n.md");
  });
  it("does not treat host download completion as verification on this device", async () => {
    const { manager } = setup();
    vi.mocked(download).mockRejectedValueOnce(
      new TransferError("Dropped", "t6", true),
    );
    const id = manager.startDownload(
      { kind: "artifact", artifact_id: "a" },
      "n.md",
    );
    await vi.waitFor(() =>
      expect(manager.get().jobs[id]?.phase).toBe("paused"),
    );
    manager.observe([
      {
        transfer_id: "t6",
        direction: "host_to_phone",
        name: "n.md",
        size: 5,
        sha256: "expected-sha",
        transferred: 5,
        state: { state: "completed" },
        destination: null,
        host_path: "/Inbox/n.md",
        created_at: 0,
        updated_at: 1,
      },
    ]);
    expect(manager.get().jobs[id]?.phase).toBe("paused");
    expect(manager.get().jobs[id]?.sha256).toBeNull();
  });
  it("never resumes an upload whose publication is unconfirmed, and clears it only as a check", async () => {
    const { manager, session } = setup();
    vi.mocked(upload).mockRejectedValueOnce(
      new TransferError("Dropped", "t7", true),
    );
    const id = manager.startUpload("n.md", new Blob(["notes"]), {
      kind: "inbox",
    });
    await vi.waitFor(() =>
      expect(manager.get().jobs[id]?.phase).toBe("paused"),
    );
    const record: TransferRecord = {
      transfer_id: "t7",
      direction: "phone_to_host",
      name: "n.md",
      size: 5,
      sha256: "expected-sha",
      transferred: 5,
      state: { state: "reconciliation_required", reason: "Unconfirmed" },
      destination: { kind: "inbox" },
      publication: { root: "/home/sam/Inbox", path: "n (2).md" },
      host_path: null,
      created_at: 0,
      updated_at: 1,
    };
    session.state.set({ ...session.state.get(), transfers: { t7: record } });
    manager.observe([record]);
    expect(manager.get().jobs[id]?.resumable).toBe(false);
    manager.continue(id);
    expect(vi.mocked(upload)).toHaveBeenCalledTimes(1);
    const command = vi.spyOn(session, "command").mockResolvedValue({
      command_id: "c",
      status: "accepted",
      seq: 1,
      result: null,
      error: null,
    });
    vi.spyOn(session, "query").mockResolvedValue({
      kind: "command_status",
      receipt: null,
      stage: "host_accepted",
    });
    const cancelled = manager.cancel(id);
    await vi.waitFor(() => expect(command).toHaveBeenCalled());
    expect(manager.get().jobs[id]?.phase).not.toBe("cancelled");
    session.state.set({
      ...session.state.get(),
      commandStages: { c: { stage: "observed", at: 1, detail: null } },
    });
    await cancelled;
    expect(command).toHaveBeenCalledWith(
      { command: "cancel_transfer", transfer_id: "t7" },
      expect.anything(),
    );
    expect(manager.get().jobs[id]?.phase).toBe("cancelled");
  });
});

it("declares the reviewed collision choice before sending and resumes that declaration", async () => {
  const { manager, session } = setup();
  const command = vi.spyOn(session, "command").mockResolvedValue({
    command_id: "review",
    status: "accepted",
    seq: 1,
    error: null,
    result: {
      kind: "transfer",
      transfer: {
        transfer_id: "reviewed-transfer",
        direction: "phone_to_host",
        name: "notes (1).md",
        size: 5,
        sha256: "expected-sha",
        transferred: 0,
        state: { state: "pending" },
        destination: { kind: "inbox" },
        host_path: null,
        created_at: 0,
        updated_at: 0,
      },
    },
  });
  vi.mocked(upload).mockResolvedValue({
    transferId: "reviewed-transfer",
    sha256: "expected-sha",
    artifactId: null,
  });
  const id = manager.startUpload(
    "notes.md",
    new Blob(["draft"]),
    { kind: "inbox" },
    { choice: "keep_both", name: "notes (1).md" },
  );
  await vi.waitFor(() =>
    expect(manager.get().jobs[id]?.phase).toBe("completed"),
  );
  expect(command.mock.calls[0]?.[0]).toMatchObject({
    command: "create_upload_reviewed",
    collision: { choice: "keep_both", name: "notes (1).md" },
  });
  expect(vi.mocked(upload).mock.calls[0]?.[1].transferId).toBe(
    "reviewed-transfer",
  );
});

describe("bounded progress persistence", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });
  const key = "transfers.v1.p";
  const stored = (): Record<string, TransferJob> => JSON.parse(localStorage.getItem(key)!);
  const history = () => Object.fromEntries(Array.from({ length: 250 }, (_, i) => [String(i), {
    id: String(i), name: `photo-${i}.jpg`, direction: "upload", size: 1024,
    transferred: 1024, phase: "completed", createdAt: 1, updatedAt: 2,
    transferId: `t-${i}`, sha256: "expected-sha", destination: { kind: "inbox" },
    source: null, artifactId: null, hostPath: null, error: null, storageError: null, resumable: false,
  } satisfies TransferJob]));

  async function active(direction: "upload" | "download", size = 300, manager = setup().manager) {
    // The transport and byte store are mocked: simulate a large file without
    // allocating it or measuring phone storage latency.
    const blob = new Blob(["mock bytes"]);
    Object.defineProperty(blob, "size", { value: size });
    const result = { transferId: "active", sha256: "expected-sha", artifactId: null, blob };
    let finish!: () => void, fail!: (e: unknown) => void;
    const promise = new Promise<typeof result>((resolve, reject) => {
      finish = () => resolve(result);
      fail = reject;
    });
    let progress!: (transferred: number, size: number) => void;
    if (direction === "upload")
      vi.mocked(upload).mockImplementationOnce(async (_ctx, o) => {
        // Recovery identity must already be saved before declaring or sending.
        expect(Object.values(stored()).find((j) => j.declaration === o.commandId)).toBeDefined();
        await o.onDeclared?.("active");
        progress = o.onProgress!;
        return promise;
      });
    else
      vi.mocked(download).mockImplementationOnce(async (_ctx, o) => {
        o.onDeclared?.("active", size);
        progress = o.onProgress!;
        return promise;
      });
    const id = direction === "upload"
      ? manager.startUpload("large.bin", blob, { kind: "inbox" })
      : manager.startDownload({ kind: "artifact", artifact_id: "a" }, "large.bin");
    await vi.advanceTimersByTimeAsync(0);
    expect(stored()[id]).toMatchObject({ transferId: "active", phase: "sending" });
    return { manager, id, progress, finish, fail };
  }

  it.each(["upload", "download"] as const)("coalesces 4096 %s chunks with 250 saved jobs", async (direction) => {
    const old = history();
    localStorage.setItem(key, JSON.stringify(old));
    const { manager } = setup();
    const writes = vi.spyOn(Storage.prototype, "setItem"), notify = vi.fn();
    manager.subscribe(notify);
    const transfer = await active(direction, 1024 ** 3, manager);
    const before = manager.get(), saves = writes.mock.calls.length, notifications = notify.mock.calls.length;
    for (let chunk = 1; chunk <= 4096; chunk++) transfer.progress(chunk * 256 * 1024, 1024 ** 3);
    await vi.advanceTimersByTimeAsync(249);
    expect(writes).toHaveBeenCalledTimes(saves);
    expect(notify).toHaveBeenCalledTimes(notifications);
    expect(manager.get()).toBe(before);
    expect(before.jobs[transfer.id]?.transferred).toBe(0);
    await vi.advanceTimersByTimeAsync(1);
    expect(writes).toHaveBeenCalledTimes(saves + 1);
    expect(notify).toHaveBeenCalledTimes(notifications + 1);
    expect(stored()[transfer.id]).toMatchObject({ transferred: 1024 ** 3, phase: "verifying" });
    transfer.finish();
    await vi.advanceTimersByTimeAsync(0);
    expect(stored()[transfer.id]).toMatchObject({ transferred: 1024 ** 3, phase: "completed" });
    expect(setup().manager.get().jobs[transfer.id]?.phase).toBe("completed");
    for (const [id, job] of Object.entries(old)) expect(stored()[id]).toEqual(job);
    expect(writes.mock.calls.length).toBeLessThanOrEqual(10);
    expect(notify.mock.calls.length).toBeLessThanOrEqual(10);
    expect(writes.mock.calls.reduce((bytes, [, json]) => bytes + json.length, 0)).toBeLessThan(1024 ** 2);
  });

  it("keeps a fixed cadence across concurrent jobs during continuous progress", async () => {
    const first = await active("upload", 5000);
    const second = await active("download", 5000, first.manager);
    const writes = vi.spyOn(Storage.prototype, "setItem"), notify = vi.fn();
    first.manager.subscribe(notify);
    for (let elapsed = 0; elapsed < 1000; elapsed += 10) {
      first.progress(elapsed + 1, 5000);
      second.progress(elapsed + 2, 5000);
      await vi.advanceTimersByTimeAsync(10);
      expect(writes).toHaveBeenCalledTimes(Math.floor((elapsed + 10) / 250));
    }
    expect(notify).toHaveBeenCalledTimes(4);
    expect(stored()[first.id]?.transferred).toBe(991);
    expect(stored()[second.id]?.transferred).toBe(992);
    first.finish(); second.finish();
    await vi.advanceTimersByTimeAsync(0);
  });

  it.each(["pause", "failure", "completion"])("flushes pending counters on %s without a stale timer", async (transition) => {
    const transfer = await active("upload");
    transfer.progress(123, 300);
    if (transition === "pause") {
      transfer.manager.pause(transfer.id);
      expect(stored()[transfer.id]?.transferred).toBe(123);
      transfer.fail(new TransferError("Paused", "active", true));
    } else if (transition === "failure") transfer.fail(new TransferError("Failed", "active", false));
    else transfer.finish();
    await vi.advanceTimersByTimeAsync(0);
    const phase = transition === "pause" ? "paused" : transition === "failure" ? "failed" : "completed";
    expect(stored()[transfer.id]).toMatchObject({ phase, transferred: transition === "completion" ? 300 : 123 });
    const writes = vi.spyOn(Storage.prototype, "setItem");
    // Even a late callback cannot overwrite a terminal state.
    transfer.progress(200, 300);
    await vi.advanceTimersByTimeAsync(1000);
    expect(writes).not.toHaveBeenCalled();
    expect(transfer.manager.get().jobs[transfer.id]?.phase).toBe(phase);
  });

  it.each(["visibilitychange", "pagehide"])("flushes on %s before timers run and restores a resumable job", async (event) => {
    const transfer = await active("download");
    transfer.progress(123, 300);
    if (event === "visibilitychange") {
      vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
      document.dispatchEvent(new Event(event));
    } else window.dispatchEvent(new Event(event));
    expect(stored()[transfer.id]?.transferred).toBe(123);
    expect(setup().manager.get().jobs[transfer.id]).toMatchObject({
      phase: "paused", resumable: true, transferId: "active", transferred: 123,
    });
    const writes = vi.spyOn(Storage.prototype, "setItem");
    await vi.advanceTimersByTimeAsync(1000);
    expect(writes).not.toHaveBeenCalled();
    transfer.finish();
    await vi.advanceTimersByTimeAsync(0);
  });

  it("flushes every job's counters with durable changes and preserves newer counters", async () => {
    const first = await active("upload");
    const second = await active("download", 300, first.manager);
    first.progress(100, 300); second.progress(200, 300);
    first.manager.saved(second.id, "Downloads/large.bin");
    expect(stored()[first.id]?.transferred).toBe(100);
    expect(stored()[second.id]).toMatchObject({ transferred: 200, savedAs: "Downloads/large.bin" });
    first.progress(150, 300);
    await vi.advanceTimersByTimeAsync(250);
    expect(stored()[first.id]?.transferred).toBe(150);
    expect(stored()[second.id]?.savedAs).toBe("Downloads/large.bin");
    first.finish(); second.finish();
    await vi.advanceTimersByTimeAsync(0);
  });

  it("reports a failed progress save and retries with the latest counters", async () => {
    const transfer = await active("download");
    const writes = vi.spyOn(Storage.prototype, "setItem").mockImplementationOnce(() => { throw new Error("Quota exceeded"); });
    transfer.progress(100, 300);
    await vi.advanceTimersByTimeAsync(250);
    expect(transfer.manager.get().storageError).toContain("could not be saved");
    expect(transfer.manager.get().jobs[transfer.id]?.transferred).toBe(100);
    expect(stored()[transfer.id]?.transferred).toBe(0);
    transfer.progress(200, 300);
    await vi.advanceTimersByTimeAsync(250);
    expect(writes).toHaveBeenCalledTimes(2);
    expect(transfer.manager.get().storageError).toBeNull();
    expect(stored()[transfer.id]?.transferred).toBe(200);
    transfer.finish();
    await vi.advanceTimersByTimeAsync(0);
  });
});

describe("saved history that can't be read", () => {
  afterEach(() => vi.restoreAllMocks());
  const saved = JSON.stringify({
    old: {
      id: "old",
      name: "photo.jpg",
      direction: "download",
      size: 4,
      transferred: 4,
      phase: "completed",
      createdAt: 1,
      updatedAt: 2,
      transferId: "t-old",
      sha256: "old-sha",
      destination: null,
      source: { kind: "artifact", artifact_id: "a-old" },
      artifactId: null,
      hostPath: null,
      error: null,
      storageError: null,
      resumable: false,
    },
  });
  /** The manager's next `failures` reads of its history throw. */
  function failReads(failures: number) {
    const getItem = Storage.prototype.getItem;
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(function (
      this: Storage,
      key: string,
    ) {
      if (key === "transfers.v1.p" && failures-- > 0)
        throw new Error("Storage is unavailable");
      return getItem.call(this, key);
    });
  }
  function startDownload(manager: TransferManager) {
    vi.mocked(download).mockReturnValueOnce(new Promise(() => {}));
    return manager.startDownload(
      { kind: "artifact", artifact_id: "a-new" },
      "new.png",
    );
  }
  it("leaves the saved history untouched and says so while it stays unread", () => {
    localStorage.setItem("transfers.v1.p", saved);
    failReads(Infinity);
    const { manager } = setup();
    expect(manager.get().jobs.old).toBeUndefined();
    expect(manager.get().storageError).toContain("could not be read");
    const id = startDownload(manager);
    expect(manager.get().jobs[id]?.phase).toBe("preparing");
    expect(manager.get().storageError).toContain("could not be read");
    vi.restoreAllMocks();
    expect(localStorage.getItem("transfers.v1.p")).toBe(saved);
  });
  it("sets corrupt history aside and can find a new completed download after restart", async () => {
    localStorage.setItem("transfers.v1.p", "{not json");
    const { manager } = setup();
    expect(manager.get().storageError).toBeNull();
    expect(manager.get().storageRecovery).toContain("was damaged");
    expect(localStorage.getItem("transfers.v1.p.unreadable")).toBe("{not json");
    const blob = new Blob(["new download"]);
    vi.mocked(download).mockResolvedValueOnce({ transferId: "t-new", sha256: "verified", blob });
    const id = manager.startDownload({ kind: "artifact", artifact_id: "a-new" }, "new.png");
    await vi.waitFor(() => expect(manager.get().jobs[id]?.phase).toBe("completed"));
    expect(transferBytes.put).toHaveBeenCalledWith(`p:${id}`, blob);
    vi.mocked(transferBytes.get).mockResolvedValueOnce(blob);
    const restored = setup().manager;
    expect(restored.get().jobs[id]?.phase).toBe("completed");
    expect(restored.get().storageRecovery).toBeNull();
    expect(await restored.blob(id)).toBe(blob);
    expect(transferBytes.get).toHaveBeenCalledWith(`p:${id}`);
    expect(localStorage.getItem("transfers.v1.p.unreadable")).toBe("{not json");
  });
  it("keeps damaged history untouched until its backup can be written", () => {
    localStorage.setItem("transfers.v1.p", "{not json");
    const write = Storage.prototype.setItem;
    const writes = vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (
      this: Storage, key: string, value: string,
    ) {
      if (key === "transfers.v1.p.unreadable") throw new Error("Quota exceeded");
      write.call(this, key, value);
    });
    const { manager, session } = setup();
    const id = startDownload(manager);
    expect(manager.get().storageError).toContain("could not be set aside");
    expect(manager.get().storageRecovery).toBeNull();
    expect(localStorage.getItem("transfers.v1.p")).toBe("{not json");
    expect(writes.mock.calls.every(([key]) => key === "transfers.v1.p.unreadable")).toBe(true);
    writes.mockRestore();
    session.link.set({ state: "connecting", attempt: 1 });
    expect(manager.get().storageError).toBeNull();
    expect(manager.get().storageRecovery).toContain("was damaged");
    expect(localStorage.getItem("transfers.v1.p.unreadable")).toBe("{not json");
    expect(setup().manager.get().jobs[id]?.name).toBe("new.png");
  });
  it("reads it again before saving and keeps both", () => {
    localStorage.setItem("transfers.v1.p", saved);
    failReads(1);
    const { manager } = setup();
    expect(manager.get().jobs.old).toBeUndefined();
    const id = startDownload(manager);
    expect(manager.get().jobs.old?.phase).toBe("completed");
    expect(manager.get().jobs[id]?.phase).toBe("preparing");
    expect(manager.get().storageError).toBeNull();
    const stored = JSON.parse(localStorage.getItem("transfers.v1.p")!);
    expect(Object.keys(stored).sort()).toEqual(["old", id].sort());
  });
  it("reads it again when the connection changes", () => {
    localStorage.setItem("transfers.v1.p", saved);
    failReads(1);
    const { manager, session } = setup();
    expect(manager.get().storageError).toContain("could not be read");
    session.link.set({ state: "connecting", attempt: 1 });
    expect(manager.get().jobs.old?.phase).toBe("completed");
    expect(manager.get().storageError).toBeNull();
  });
});
