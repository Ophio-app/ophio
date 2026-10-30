// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it, vi } from "vitest";
import type { TransferClientFrame, TransferRecord } from "../protocol/types";
import type { Platform, TransferEvent, TransferLink } from "./link";
import type { HostSession } from "./session";
import { ANDROID_SAVE_LIMIT, CHUNK_BYTES, TransferError, download, sha256Hex, upload } from "./transfer";

const BYTES = new Uint8Array(CHUNK_BYTES * 2 + 1234).map((_, i) => (i * 31) % 251);
// Compare ArrayBuffers with toStrictEqual below: it checks every byte without
// the large Object.entries arrays that toEqual builds for Uint8Array fixtures.

function record(id: string, size: number, sha256: string | null): TransferRecord {
  return {
    transfer_id: id,
    direction: "host_to_phone",
    name: "notes.md",
    size,
    sha256,
    transferred: 0,
    state: { state: "pending" },
    destination: null,
    host_path: null,
    created_at: 0,
    updated_at: 0,
  };
}

/** A computer that serves `bytes` for downloads and stores uploads. */
class FakeComputer {
  /** Upload bytes retained across links, including any seeded partial upload. */
  received: Uint8Array[] = [];
  /** Drop the link after this many download chunks, once. */
  dropAfter: number | null = null;
  /** Serve different bytes than declared. */
  corrupt = false;
  /** Deliver the download header, chunks and completion in one callback. */
  batched = false;
  /** Override the offset accepted by the computer. */
  startAt: number | null = null;

  constructor(readonly bytes: Uint8Array) {}

  link(): TransferLink {
    const listeners = new Set<(e: TransferEvent) => void>();
    const emit = (...events: TransferEvent[]) => {
      const deliver = (e: TransferEvent) => listeners.forEach((l) => l(e));
      if (this.batched) setTimeout(() => events.forEach(deliver), 0);
      else for (const e of events) setTimeout(() => deliver(e), 0);
    };
    return {
      send: (f: TransferClientFrame) => {
        if (f.type === "upload") {
          const offset = this.received.reduce((size, chunk) => size + chunk.byteLength, 0);
          emit({ type: "text", frame: { type: "resume_at", transfer_id: f.transfer_id, offset } });
        }
        if (f.type === "finish")
          void sha256Hex(new Blob(this.received as Uint8Array<ArrayBuffer>[])).then((sha) =>
            emit({ type: "text", frame: { type: "completed", transfer_id: f.transfer_id, sha256: sha, artifact_id: "a1" } }),
          );
        if (f.type === "download") void this.#serve(f.transfer_id, this.startAt ?? f.offset, emit);
        return true;
      },
      chunk: async (bytes) => {
        this.received.push(bytes.slice());
        return true;
      },
      close: () => listeners.clear(),
      listen: (l) => {
        listeners.add(l);
        emit({ type: "state", state: { state: "online", since: 0 } });
        return () => listeners.delete(l);
      },
    };
  }

  async #serve(id: string, offset: number, emit: (...events: TransferEvent[]) => void): Promise<void> {
    const sha = await sha256Hex(new Blob([this.bytes as Uint8Array<ArrayBuffer>]));
    const events: TransferEvent[] = [
      { type: "text", frame: { type: "sending", transfer_id: id, offset, size: this.bytes.length, sha256: sha } },
    ];
    let n = 0;
    for (let at = offset; at < this.bytes.length; at += CHUNK_BYTES) {
      if (this.dropAfter !== null && n++ === this.dropAfter) {
        this.dropAfter = null;
        events.push({ type: "state", state: { state: "offline", since: 0, reason: "gone", retryAt: null } });
        emit(...events);
        return;
      }
      const data = this.bytes.slice(at, at + CHUNK_BYTES);
      if (this.corrupt) data.fill(0, 0, 1);
      events.push({ type: "chunk", data });
    }
    events.push({ type: "text", frame: { type: "completed", transfer_id: id, sha256: sha, artifact_id: null } });
    emit(...events);
  }

  ctx() {
    const session = {
      command: async (body: { command: string; size?: number; sha256?: string }) => ({
        command_id: "c1",
        status: "accepted",
        seq: 1,
        error: null,
        result: {
          kind: "transfer",
          transfer: record("t1", body.size ?? this.bytes.length, body.sha256 ?? null),
        },
      }),
    } as unknown as HostSession;
    const platform = { transfer: () => this.link() } as unknown as Platform;
    return { platform, profileId: "p", session };
  }
}

describe("transfers", () => {
  it("uploads in bounded chunks and verifies the computer's hash", async () => {
    const pc = new FakeComputer(BYTES);
    const sent: number[] = [];
    const r = await upload(pc.ctx(), {
      name: "x.bin",
      blob: new Blob([BYTES]),
      destination: { kind: "artifact" },
      label: "Send",
      onProgress: (n) => sent.push(n),
    });
    expect(r.artifactId).toBe("a1");
    expect(pc.received.every((c) => c.byteLength <= CHUNK_BYTES)).toBe(true);
    expect(sent.at(-1)).toBe(BYTES.length);
  });

  it.each([CHUNK_BYTES, CHUNK_BYTES + 17])("resumes an existing upload from byte %s without resending retained bytes", async (saved) => {
    const pc = new FakeComputer(BYTES);
    pc.received.push(BYTES.slice(0, saved));
    const ctx = pc.ctx();
    const command = vi.spyOn(ctx.session, "command");
    const link = pc.link();
    const frames = vi.spyOn(link, "send");
    ctx.platform.transfer = () => link;
    const blob = new Blob([BYTES]);
    const sha256 = await sha256Hex(blob);
    const progress: [number, number][] = [];

    const result = await upload(ctx, {
      name: "x.bin",
      blob,
      destination: { kind: "artifact" },
      label: "Continue",
      transferId: "existing-upload",
      onProgress: (sent, size) => progress.push([sent, size]),
    });

    expect(command).not.toHaveBeenCalled();
    expect(frames.mock.calls).toEqual([
      [{ type: "upload", transfer_id: "existing-upload" }],
      [{ type: "finish", transfer_id: "existing-upload" }],
    ]);
    const remaining = pc.received.slice(1);
    expect(remaining.map((chunk) => chunk.byteLength)).toEqual([CHUNK_BYTES, BYTES.length - saved - CHUNK_BYTES]);
    expect(await new Blob(remaining as Uint8Array<ArrayBuffer>[]).arrayBuffer()).toStrictEqual(BYTES.slice(saved).buffer);
    const assembled = new Blob(pc.received as Uint8Array<ArrayBuffer>[]);
    expect(await assembled.arrayBuffer()).toStrictEqual(BYTES.buffer);
    expect(await sha256Hex(assembled)).toBe(sha256);
    expect(result).toEqual({ transferId: "existing-upload", sha256, artifactId: "a1" });
    expect(progress).toEqual([[saved + CHUNK_BYTES, BYTES.length], [BYTES.length, BYTES.length]]);
  });

  it("downloads and verifies the bytes", async () => {
    const pc = new FakeComputer(BYTES);
    const r = await download(pc.ctx(), { source: { kind: "artifact", artifact_id: "a1" }, label: "Get" });
    expect(await r.blob.arrayBuffer()).toStrictEqual(BYTES.buffer);
    expect(r.sha256).toBe(await sha256Hex(new Blob([BYTES])));
  });

  it.each([
    { name: "fresh", saved: 0, startAt: null },
    { name: "resumed", saved: CHUNK_BYTES, startAt: null },
    { name: "restarted from zero", saved: CHUNK_BYTES, startAt: 0 },
  ])("verifies a $name download delivered in one callback", async ({ saved, startAt }) => {
    const pc = new FakeComputer(BYTES);
    pc.batched = true;
    pc.startAt = startAt;
    const progress: [number, number][] = [];
    const r = await download(pc.ctx(), {
      source: { kind: "artifact", artifact_id: "a1" },
      label: "Get",
      ...(saved ? { resume: { transferId: "t1", received: new Blob([BYTES.slice(0, saved)]) } } : {}),
      onProgress: (received, size) => progress.push([received, size]),
    });
    expect(await r.blob.arrayBuffer()).toStrictEqual(BYTES.buffer);
    expect(r.sha256).toBe(await sha256Hex(new Blob([BYTES])));
    expect(progress[0]).toEqual([startAt ?? saved, BYTES.length]);
    expect(progress.at(-1)).toEqual([BYTES.length, BYTES.length]);
    expect(progress.every(([received, size], i) =>
      size === BYTES.length && received >= (progress[i - 1]?.[0] ?? 0) && received <= size,
    )).toBe(true);
  });

  it.each([false, true])("resumes a dropped download from the bytes already received (batched: %s)", async (batched) => {
    const pc = new FakeComputer(BYTES);
    pc.batched = batched;
    pc.dropAfter = 1;
    const ctx = pc.ctx();
    const first = await download(ctx, { source: { kind: "artifact", artifact_id: "a1" }, label: "Get" }).catch((e: unknown) => e);
    expect(first).toBeInstanceOf(TransferError);
    const err = first as TransferError;
    expect(err.resumable).toBe(true);
    // The link's short reason reads as its own sentence.
    expect(err.message).toBe("gone. Resolve this, then retry to continue where it stopped.");
    expect(err.partial?.size).toBe(CHUNK_BYTES);
    expect(await err.partial!.arrayBuffer()).toStrictEqual(BYTES.slice(0, CHUNK_BYTES).buffer);
    const r = await download(ctx, {
      source: { kind: "artifact", artifact_id: "a1" },
      label: "Get",
      resume: { transferId: err.transferId ?? "", received: err.partial ?? new Blob() },
    });
    expect(await r.blob.arrayBuffer()).toStrictEqual(BYTES.buffer);
    expect(r.sha256).toBe(await sha256Hex(new Blob([BYTES])));
  });

  it("doesn't start a download larger than the phone takes", async () => {
    const pc = new FakeComputer(BYTES);
    const ctx = pc.ctx();
    const sent: string[] = [];
    ctx.session = {
      command: async (body: { command: string }) => {
        sent.push(body.command);
        return {
          command_id: "c1",
          status: "accepted",
          seq: 1,
          error: null,
          result: { kind: "transfer", transfer: record("t1", 1.5 * 1024 ** 3, "a".repeat(64)) },
        };
      },
    } as unknown as HostSession;
    ctx.platform.downloadLimit = ANDROID_SAVE_LIMIT;
    ctx.platform.transfer = () => {
      throw new Error("No bytes are asked for.");
    };
    const e = await download(ctx, { source: { kind: "artifact", artifact_id: "a1" }, label: "Get" }).catch((x: unknown) => x);
    expect(e).toBeInstanceOf(TransferError);
    expect((e as TransferError).resumable).toBe(false);
    expect((e as TransferError).message).toBe(
      "“notes.md” is 1.5 GB. This phone takes files up to 1 GB, so fetch this one another way, such as on the computer itself.",
    );
    // The transfer the computer declared is cancelled there.
    await vi.waitFor(() => expect(sent).toEqual(["create_download", "cancel_transfer"]));
  });

  it.each([false, true])("refuses bytes that don't match the declared hash (batched: %s)", async (batched) => {
    const pc = new FakeComputer(BYTES);
    pc.batched = batched;
    pc.corrupt = true;
    const e = await download(pc.ctx(), { source: { kind: "artifact", artifact_id: "a1" }, label: "Get" }).catch((x: unknown) => x);
    expect(e).toBeInstanceOf(TransferError);
    expect((e as TransferError).resumable).toBe(false);
    expect((e as TransferError).message).toBe("The file changed or was damaged on the way.");
  });

  it("refuses a batched resume from an unexpected nonzero offset", async () => {
    const pc = new FakeComputer(BYTES);
    pc.batched = true;
    pc.startAt = CHUNK_BYTES + 1;
    await expect(download(pc.ctx(), {
      source: { kind: "artifact", artifact_id: "a1" },
      label: "Get",
      resume: { transferId: "t1", received: new Blob([BYTES.slice(0, CHUNK_BYTES)]) },
    })).rejects.toMatchObject({ message: "The computer couldn’t continue this download.", resumable: false, partial: null });
  });
});

it.each(["upload", "download"] as const)("keeps a fatal refusal for a Library %s", async direction => {
  const pc = new FakeComputer(BYTES);
  const ctx = pc.ctx();
  ctx.platform.transfer = () => ({
    send: () => false, chunk: async () => false, close() {},
    listen(listener) {
      // Even a refusal already known when listen starts must survive until online().
      listener({ type: "state", state: { state: "offline", since: 0, code: "not_found", reason: "Transfer no longer exists on studio.", retryAt: null } });
      return () => {};
    },
  });
  const result = direction === "upload"
    ? upload(ctx, { label: "Upload", blob: new Blob([BYTES]), name: "notes.md", destination: { kind: "artifact" } })
    : download(ctx, { label: "Download", source: { kind: "artifact", artifact_id: "a1" } });
  await expect(result).rejects.toMatchObject({ message: "Transfer no longer exists on studio. Start a new transfer.", resumable: false });
});
