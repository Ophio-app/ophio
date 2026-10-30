// SPDX-License-Identifier: Apache-2.0
// @vitest-environment node
// Use Node's real Blob: jsdom lacks arrayBuffer, and integrity checks must read
// the assembled bytes rather than a fixture returned by a Blob mock.
import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { sha256Hex } from "../../host/transfer";
import { nativeTrim } from "./nativeTrim";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const bytes = new Uint8Array([0, 1, 127, 128, 255, 42, 17, 99, 3, 200, 5]);
const readSize = 4;
const blob = new Blob(["original recording"]);
beforeEach(() => {
  vi.resetAllMocks();
});
async function replies(
  returnedBytes = bytes,
  range = { startMs: 1000, endMs: 5000 },
) {
  // Metadata always describes the uncorrupted clip, even when reads change.
  const hash = await sha256Hex(new Blob([bytes]));
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "clip_begin") return "job";
    if (command === "clip_trim")
      return { ...range, size: bytes.length, sha256: hash };
    if (command === "clip_read") {
      const { offset } = args as { offset: number };
      return returnedBytes.slice(offset, offset + readSize).buffer;
    }
    if (command === "clip_write" || command === "clip_discard") return;
    throw new Error(`Unexpected command: ${command}`);
  });
}
it("keeps Android’s actual snapped range and verifies returned bytes", async () => {
  await replies();
  const result = await nativeTrim(
    blob,
    { start: 2, end: 5 },
    new AbortController().signal,
  );
  expect(result.range).toEqual({ start: 1, end: 5 });
  expect(result.blob.size).toBe(bytes.length);
  expect(result.blob.type).toBe("video/mp4");
  expect(new Uint8Array(await result.blob.arrayBuffer())).toEqual(bytes);
  expect(
    vi.mocked(invoke).mock.calls.filter(([command]) => command === "clip_read"),
  ).toEqual([
    ["clip_read", { id: "job", offset: 0 }],
    ["clip_read", { id: "job", offset: 4 }],
    ["clip_read", { id: "job", offset: 8 }],
  ]);
  expect(invoke).toHaveBeenLastCalledWith("clip_discard", { id: "job" });
});
it.each([
  ["one changed byte", new Uint8Array([9, ...bytes.slice(1)])],
  ["zero-filled chunks", new Uint8Array(bytes.length)],
  [
    "a duplicated chunk",
    new Uint8Array([
      ...bytes.slice(0, 4), ...bytes.slice(0, 4), ...bytes.slice(8),
    ]),
  ],
  [
    "reordered chunks",
    new Uint8Array([
      ...bytes.slice(4, 8), ...bytes.slice(0, 4), ...bytes.slice(8),
    ]),
  ],
])(
  "rejects %s with the correct metadata hash and disposes staging",
  async (_, corrupted) => {
    await replies(corrupted);
    await expect(
      nativeTrim(blob, { start: 2, end: 5 }, new AbortController().signal),
    ).rejects.toThrow("changed during import");
    expect(invoke).toHaveBeenLastCalledWith("clip_discard", { id: "job" });
  },
);
it("refuses a fabricated future start and cleans staging on cancellation", async () => {
  await replies(bytes, { startMs: 2500, endMs: 5000 });
  await expect(
    nativeTrim(blob, { start: 2, end: 5 }, new AbortController().signal),
  ).rejects.toThrow("invalid clip range");
  expect(invoke).not.toHaveBeenCalledWith("clip_read", expect.anything());
  const cancel = new AbortController();
  cancel.abort();
  await expect(
    nativeTrim(blob, { start: 2, end: 5 }, cancel.signal),
  ).rejects.toThrow();
  expect(invoke).toHaveBeenLastCalledWith("clip_discard", { id: "job" });
});
