// SPDX-License-Identifier: Apache-2.0
import { invoke } from "@tauri-apps/api/core";
import { sha256Hex } from "../../host/transfer";
import { DerivativeError } from "./derivatives";
import type { ClipRange } from "./trim";

interface NativeClip {
  startMs: number;
  endMs: number;
  size: number;
  sha256: string;
}
export async function nativeTrim(
  blob: Blob,
  range: ClipRange,
  signal: AbortSignal,
): Promise<{ blob: Blob; range: ClipRange }> {
  const sha256 = await sha256Hex(blob);
  const id = await invoke<string>("clip_begin", { size: blob.size, sha256 });
  try {
    for (let offset = 0; offset < blob.size; offset += 256 * 1024) {
      signal.throwIfAborted();
      await invoke(
        "clip_write",
        await blob.slice(offset, offset + 256 * 1024).arrayBuffer(),
        { headers: { "x-clip-id": id } },
      );
    }
    signal.throwIfAborted();
    const result = await invoke<NativeClip>("clip_trim", {
      id,
      startMs: Math.round(range.start * 1000),
      endMs: Math.round(range.end * 1000),
    });
    if (
      !Number.isSafeInteger(result.size) ||
      result.size <= 0 ||
      result.size > 128 * 1024 * 1024 ||
      !Number.isFinite(result.startMs) ||
      !Number.isFinite(result.endMs) ||
      result.startMs < 0 ||
      result.startMs > range.start * 1000 ||
      result.endMs <= result.startMs ||
      result.endMs > range.end * 1000 + 1000
    )
      throw new DerivativeError(
        "Android returned an invalid clip range. Use screenshots instead.",
      );
    const parts: ArrayBuffer[] = [];
    for (let offset = 0; offset < result.size;) {
      signal.throwIfAborted();
      const chunk = await invoke<ArrayBuffer>("clip_read", { id, offset });
      if (
        !chunk.byteLength ||
        chunk.byteLength > 256 * 1024 ||
        offset + chunk.byteLength > result.size
      )
        throw new DerivativeError("The trimmed clip is incomplete. Try again.");
      parts.push(chunk);
      offset += chunk.byteLength;
    }
    const output = new Blob(parts, { type: "video/mp4" });
    if ((await sha256Hex(output)) !== result.sha256)
      throw new DerivativeError(
        "The trimmed clip changed during import. Try again.",
      );
    return {
      blob: output,
      range: { start: result.startMs / 1000, end: result.endMs / 1000 },
    };
  } finally {
    await invoke("clip_discard", { id });
  }
}
