// SPDX-License-Identifier: Apache-2.0
import { isTauri } from "@tauri-apps/api/core";
import { DerivativeError } from "./derivatives";
import { encodePng } from "./sources";
import { webm, type VideoPacket } from "./webm";
import type { ClipRange } from "./trim";

export const MAX_CLIP_BYTES = 128 * 1024 * 1024;
export const MAX_CLIP_SECONDS = 60;
export interface ClipOutput {
  blob: Blob;
  range: ClipRange;
  method: "android-remux" | "webcodecs";
  frames: { blob: Blob; atMs: number }[];
}
export const frameTimes = (range: ClipRange): number[] => {
  const span = range.end - range.start;
  return [
    range.start,
    range.start + span / 2,
    range.end - Math.min(1 / 15, span / 4),
  ];
};
export function validateClipRange(blob: Blob, range: ClipRange): void {
  if (!blob.size || blob.size > MAX_CLIP_BYTES)
    throw new DerivativeError("Choose a clip smaller than 128 MB.");
  if (
    ![range.start, range.end].every(Number.isFinite) ||
    range.start < 0 ||
    range.end <= range.start ||
    range.end - range.start > MAX_CLIP_SECONDS
  )
    throw new DerivativeError(
      "Choose a range of up to 60 seconds, with its end after its start.",
    );
}

export async function openVideo(
  blob: Blob,
  signal: AbortSignal,
): Promise<{ video: HTMLVideoElement; close(): void }> {
  const video = document.createElement("video");
  const url = URL.createObjectURL(blob);
  const close = () => {
    video.removeAttribute("src");
    video.load();
    URL.revokeObjectURL(url);
  };
  video.muted = true;
  video.playsInline = true;
  video.preload = "auto";
  try {
    const ready = event(video, "loadeddata", signal);
    video.src = url;
    await ready;
    if (!video.videoWidth || !Number.isFinite(video.duration))
      throw new DerivativeError(
        "This clip’s duration or dimensions aren’t available.",
      );
    return { video, close };
  } catch (error) {
    close();
    throw error;
  }
}
function event(
  video: HTMLVideoElement,
  type: string,
  signal: AbortSignal,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const finish = (error?: Error) => {
      clearTimeout(timer);
      video.removeEventListener(type, loaded);
      video.removeEventListener("error", failed);
      signal.removeEventListener("abort", aborted);
      if (error) reject(error);
      else resolve();
    };
    const loaded = () => finish();
    const failed = () =>
      finish(
        new DerivativeError(
          "This device can’t decode this clip. Use screenshots instead.",
        ),
      );
    const aborted = () =>
      finish(
        new DerivativeError("Clip creation cancelled. Your draft is kept."),
      );
    const timer = setTimeout(
      () =>
        finish(
          new DerivativeError(
            "The clip took too long to decode. Use screenshots instead.",
          ),
        ),
      15_000,
    );
    video.addEventListener(type, loaded, { once: true });
    video.addEventListener("error", failed, { once: true });
    signal.addEventListener("abort", aborted, { once: true });
    if (signal.aborted) aborted();
  });
}
export async function seekVideo(
  video: HTMLVideoElement,
  time: number,
  signal: AbortSignal,
): Promise<void> {
  signal.throwIfAborted();
  if (
    !video.seeking &&
    video.readyState >= 2 &&
    Math.abs(video.currentTime - time) < 0.00001
  )
    return;
  const ready = event(video, "seeked", signal);
  video.currentTime = time;
  await ready;
  if (video.readyState < 2) await event(video, "loadeddata", signal);
}
export async function clipFrames(
  blob: Blob,
  range: ClipRange,
  signal: AbortSignal,
  offset = 0,
): Promise<ClipOutput["frames"]> {
  const { video, close } = await openVideo(blob, signal);
  const canvas = document.createElement("canvas");
  canvas.width = video.videoWidth;
  canvas.height = video.videoHeight;
  const ctx = canvas.getContext("2d");
  try {
    if (!ctx)
      throw new DerivativeError(
        "Frame extraction isn’t available. Use screenshots instead.",
      );
    const frames = [];
    for (const time of frameTimes(range)) {
      await seekVideo(video, time - offset, signal);
      ctx.drawImage(video, 0, 0);
      frames.push({
        blob: await encodePng(canvas),
        atMs: Math.round(time * 1000),
      });
    }
    return frames;
  } finally {
    close();
  }
}
export async function createClip(
  blob: Blob,
  range: ClipRange,
  signal: AbortSignal,
): Promise<ClipOutput> {
  validateClipRange(blob, range);
  if (isTauri()) {
    const { nativeTrim } = await import("./nativeTrim");
    const result = await nativeTrim(blob, range, signal);
    return {
      ...result,
      frames: await clipFrames(
        result.blob,
        result.range,
        signal,
        result.range.start,
      ),
      method: "android-remux",
    };
  }
  if (typeof VideoEncoder === "undefined")
    throw new DerivativeError(
      "Clip export isn’t supported on this device. Use screenshots instead.",
    );
  const { video, close } = await openVideo(blob, signal);
  let encoder: VideoEncoder | null = null;
  const abortEncoding = () => {
    if (encoder?.state !== "closed") encoder?.close();
  };
  signal.addEventListener("abort", abortEncoding, { once: true });
  try {
    signal.throwIfAborted();
    if (range.end > video.duration + 0.001)
      throw new DerivativeError("Choose a range inside this clip.");
    const scale = Math.min(
      1,
      1280 / Math.max(video.videoWidth, video.videoHeight),
    );
    const width = Math.max(2, Math.round((video.videoWidth * scale) / 2) * 2),
      height = Math.max(2, Math.round((video.videoHeight * scale) / 2) * 2);
    const config: VideoEncoderConfig = {
      codec: "vp8",
      width,
      height,
      bitrate: 3_000_000,
      framerate: 15,
      latencyMode: "realtime",
    };
    if (!(await VideoEncoder.isConfigSupported(config)).supported)
      throw new DerivativeError(
        "This device can’t encode this clip. Use screenshots instead.",
      );
    const canvas = document.createElement("canvas");
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext("2d");
    if (!ctx)
      throw new DerivativeError(
        "Video export isn’t available. Use screenshots instead.",
      );
    const packets: VideoPacket[] = [];
    let failure: DOMException | null = null,
      bytes = 0;
    const duration = Math.round((range.end - range.start) * 1000);
    const timings = new Map<number, number>();
    encoder = new VideoEncoder({
      output: (chunk) => {
        const data = new Uint8Array(chunk.byteLength);
        chunk.copyTo(data);
        bytes += data.length;
        packets.push({
          timestamp: Math.round(chunk.timestamp / 1000),
          duration: timings.get(chunk.timestamp) ?? 0,
          key: chunk.type === "key",
          bytes: data,
        });
      },
      error: (error) => {
        failure = error;
      },
    });
    encoder.configure(config);
    for (
      let index = 0, time = 0;
      time < duration;
      index++, time = Math.round((index * 1000) / 15)
    ) {
      signal.throwIfAborted();
      if (failure) throw failure;
      if (bytes > MAX_CLIP_BYTES)
        throw new DerivativeError(
          "The exported clip is too large. Choose a shorter range.",
        );
      const next = Math.min(duration, Math.round(((index + 1) * 1000) / 15));
      await seekVideo(video, range.start + time / 1000, signal);
      ctx.drawImage(video, 0, 0, width, height);
      const frame = new VideoFrame(canvas, {
        timestamp: time * 1000,
        duration: (next - time) * 1000,
      });
      timings.set(time * 1000, next - time);
      try {
        encoder.encode(frame, { keyFrame: index % 15 === 0 });
      } finally {
        frame.close();
      }
      // A bounded queue also surfaces asynchronous codec errors before more work.
      if (encoder.encodeQueueSize >= 4) await encoder.flush();
    }
    await encoder.flush();
    if (failure) throw failure;
    const output = webm(packets, width, height, duration);
    if (output.size > MAX_CLIP_BYTES)
      throw new DerivativeError(
        "The exported clip is too large. Choose a shorter range.",
      );
    const frames = await clipFrames(
      output,
      { start: 0, end: duration / 1000 },
      signal,
    );
    return {
      blob: output,
      range,
      method: "webcodecs",
      frames: frames.map((f) => ({
        ...f,
        atMs: f.atMs + Math.round(range.start * 1000),
      })),
    };
  } catch (error) {
    if (error instanceof DerivativeError || signal.aborted) throw error;
    throw new DerivativeError(
      "This device couldn’t create the clip. Use screenshots instead.",
    );
  } finally {
    signal.removeEventListener("abort", abortEncoding);
    if (encoder?.state !== "closed") encoder?.close();
    close();
  }
}
