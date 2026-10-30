// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { ControlTarget, MediaClientFrame } from "../../protocol/types";
import type { MediaEvent, MediaLink } from "../../host/link";
import { buildTileFrame } from "../../protocol/binary";
import { useMedia } from "./useMedia";

const fixture = vi.hoisted(() => ({ media: vi.fn(), close: vi.fn() }));
const platform = { media: fixture.media };
vi.mock("../../app/context", () => ({
  useApp: () => ({ platform, profile: { profileId: "recovery" } }),
  useLink: () => ({ state: "online" }),
}));
vi.mock("../../platform/awake", () => ({ useKeepAwake: () => undefined }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

it("keeps an explicitly reopened link when the host publishes availability before its first frame", () => {
  const sent: MediaClientFrame[] = [];
  const listeners = new Set<(event: MediaEvent) => void>();
  const link: MediaLink = {
    send: frame => { sent.push(frame); return true; },
    close: fixture.close,
    listen: listener => { listeners.add(listener); return () => { listeners.delete(listener); }; },
  };
  fixture.media.mockReturnValue(link);
  const target: ControlTarget = {
    target_id: "screen", kind: "output", label: "Display", native_ref: "screen",
    geometry: { width: 1280, height: 720, scale: 1.25, rotation_degrees: 0, revision: 2 },
    capture: { state: "temporarily_unavailable", reason: "Display changed, reopen to continue" },
    input: { state: "available" },
  };
  const { result, rerender } = renderHook(({ target }) => useMedia(target, true), { initialProps: { target } });
  expect(fixture.media).not.toHaveBeenCalled();
  act(() => result.current.reopen());
  expect(sent[0]).toMatchObject({ type: "open", params: { reopen: true } });
  rerender({ target: { ...target, capture: { state: "available" } } });
  expect(fixture.close).not.toHaveBeenCalled();
  act(() => {
    for (const listener of listeners) listener({ type: "text", frame: {
      type: "opened", session_id: "new-session", target_id: target.target_id,
      geometry: target.geometry, tile: 128,
    } });
  });
  expect(fixture.media).toHaveBeenCalledOnce();
  expect(fixture.close).not.toHaveBeenCalled();
  expect(result.current.picture.connected).toBe(true);
});

it("preserves a media fatal reason through the picture timeout and offers retry", async () => {
  vi.useFakeTimers();
  try {
    let listener: (event: MediaEvent) => void = () => {};
    fixture.media.mockReturnValue({ send: () => false, close() {}, listen(l: typeof listener) { listener = l; return () => {}; } });
    const target = { target_id: "screen", capture: { state: "available" } } as ControlTarget;
    const { result } = renderHook(() => useMedia(target, true));
    act(() => listener({ type: "state", state: { state: "offline", since: 0, code: "permission_denied", reason: "Screen permission was withdrawn.", retryAt: null } }));
    await act(async () => { await vi.advanceTimersByTimeAsync(9000); });
    expect(result.current.picture.error).toBe("Screen permission was withdrawn. Retry the picture after resolving this.");
    expect(result.current.picture.connected).toBe(false);
    act(() => result.current.retry());
    expect(result.current.picture.error).toBeNull();
    expect(fixture.media).toHaveBeenCalledTimes(2);
  } finally { vi.useRealTimers(); }
});

it("keeps the picture current through unchanged frames and carries it to a new picture element", async () => {
  vi.useFakeTimers();
  try {
    let listener: (event: MediaEvent) => void = () => {};
    const sent: MediaClientFrame[] = [];
    fixture.media.mockReturnValue({ send: (f: MediaClientFrame) => { sent.push(f); return true; }, close() {}, listen(l: typeof listener) { listener = l; return () => {}; } });
    const drawImage = vi.fn();
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ drawImage, clearRect() {} } as unknown as CanvasRenderingContext2D);
    const geometry = { width: 1280, height: 720, scale: 1, rotation_degrees: 0, revision: 2 };
    const target = { target_id: "screen", geometry, capture: { state: "available" } } as ControlTarget;
    const { result } = renderHook(() => useMedia(target, true));
    const first = document.createElement("canvas");
    result.current.canvas.current = first;
    act(() => listener({ type: "text", frame: { type: "opened", session_id: "s", target_id: "screen", geometry, tile: 128 } }));
    vi.stubGlobal("createImageBitmap", async () => ({ width: 128, height: 128, close() {} }));
    const tiles = [{ col: 0, row: 0, jpeg: new Uint8Array([1]) }, { col: 1, row: 0, jpeg: new Uint8Array([1]) }];
    const frame = (seq: number, keyframe: boolean) => ({ type: "binary" as const, data: buildTileFrame({ keyframe, cursor: null, tile: 128, seq, geometryRevision: 2, encWidth: 256, encHeight: 128, srcWidth: 1280, srcHeight: 720 }, keyframe ? tiles : []) });
    await act(async () => { listener(frame(1, true)); await vi.advanceTimersByTimeAsync(0); });
    expect(first.width).toBe(256);
    const drawnAt = result.current.picture.at;
    expect(drawnAt).not.toBeNull();
    // Nothing changed on the computer: a frame without tiles still renews the picture.
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); listener(frame(2, false)); await vi.advanceTimersByTimeAsync(0); });
    expect(result.current.picture.at).toBeGreaterThan(drawnAt!);
    expect(sent.filter(f => f.type === "request_keyframe")).toHaveLength(1);
    // The layout swaps the element; the new one shows the same picture at once.
    result.current.canvas.current = null;
    const second = document.createElement("canvas");
    result.current.canvas.current = second;
    expect([second.width, second.height]).toEqual([256, 128]);
    expect(drawImage).toHaveBeenCalledWith(first, 0, 0);
    expect(sent.filter(f => f.type === "request_keyframe")).toHaveLength(1);
  } finally { vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); }
});
