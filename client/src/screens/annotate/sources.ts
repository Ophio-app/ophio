// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../../brand";
import { commandResult } from "../../host/commandResult";
import { download, sha256Hex } from "../../host/transfer";
import type { Platform } from "../../host/link";
import type { HostSession } from "../../host/session";
import type {
  ArtifactMeta,
  Derivation,
  NormRect,
  SourceOrigin,
} from "../../protocol/types";
import {
  blobs,
  MAX_SOURCES,
  outgoingSources,
  type AnnotationDraft,
  type AnnotationStore,
  type SourceDraft,
} from "../../state/annotations";
import { withinRange, type ClipRange } from "./trim";
import { verifiedSource } from "./derivatives";
export type AnnotationContext = {
  platform: Platform;
  profileId: string;
  session: HostSession;
};
export class SourceError extends Error {
  override name = "SourceError";
}
/** The message already holds as many images as it can send. */
export class SourcesFullError extends SourceError {
  override name = "SourcesFullError";
}
export type ImportSource = {
  blob: Blob;
  origin: SourceOrigin;
  mediaType: string;
  width?: number;
  height?: number;
  capturedAt?: number;
  /** Keeps an image only while the message has room to send it, checked
   * as it's added, so a still placed meanwhile counts too. */
  withRoom?: boolean;
};
export async function imageSize(
  blob: Blob,
): Promise<{ width: number; height: number }> {
  try {
    const image = await createImageBitmap(blob);
    const result = { width: image.width, height: image.height };
    image.close();
    return result;
  } catch {
    throw new SourceError(
      "This image couldn’t be decoded. Choose a PNG, JPEG or WebP image.",
    );
  }
}
export async function importSource(
  store: AnnotationStore,
  id: string,
  input: ImportSource,
  extra: Partial<SourceDraft> = {},
  replaceSource?: string,
  /** Made for the person rather than by them, like a moment's still: it
   * stays through undo, reaches every version of the draft, one taken to
   * send too, and leaves the selected source where it was. A version sends
   * it only when it wants it and has room. */
  quiet?: { wants(d: AnnotationDraft): boolean },
): Promise<SourceDraft> {
  const mediaType = input.mediaType || input.blob.type;
  if (!mediaType.startsWith("image/") && !mediaType.startsWith("video/"))
    throw new SourceError("Choose an image or a video clip.");
  const dimensions = mediaType.startsWith("image/")
    ? await imageSize(input.blob)
    : await videoSize(input.blob);
  const sha256 = await sha256Hex(input.blob);
  let blobKey: string;
  try {
    blobKey = await blobs.put(input.blob);
  } catch {
    throw new SourceError(
      "Not saved on this device. There isn’t room to keep this source. Free some storage and try again.",
    );
  }
  const draft = store.get()[id];
  if (!draft) {
    await blobs.remove(blobKey);
    throw new SourceError("This draft no longer exists.");
  }
  if (
    input.withRoom &&
    mediaType.startsWith("image/") &&
    outgoingSources(draft).length >= MAX_SOURCES
  ) {
    await blobs.remove(blobKey);
    throw new SourcesFullError(`A message holds ${MAX_SOURCES} images.`);
  }
  const s: SourceDraft = {
    key: `S${draft.nextSource}`,
    blobKey,
    artifactId: null,
    sha256,
    byteSize: input.blob.size,
    mediaType,
    width: dimensions.width,
    height: dimensions.height,
    durationMs:
      "durationMs" in dimensions && typeof dimensions.durationMs === "number"
        ? dimensions.durationMs
        : null,
    origin: input.origin,
    capturedAt: input.capturedAt ?? Date.now(),
    derivedFrom: null,
    derivation: null,
    included: mediaType.startsWith("image/"),
    ...extra,
  };
  const next = draft.nextSource + 1;
  const add = (d: AnnotationDraft): AnnotationDraft => {
    const sources = d.sources.map((source) =>
      source.key === replaceSource ? { ...source, included: false } : source,
    );
    // A version already as full as a message can be has it left out, and
    // so does one with no comment waiting for it, like a send taken before
    // the comment was made.
    const sends = !quiet || (outgoingSources({ ...d, sources }).length < MAX_SOURCES && quiet.wants(d));
    return {
      ...d,
      sources: [...sources, sends ? s : { ...s, included: false }],
      nextSource: Math.max(d.nextSource, next),
      ...(quiet ? {} : { selectedSource: s.key }),
    };
  };
  if (quiet) store.place(id, add);
  else store.update(id, add);
  return s;
}
export async function captureSource(
  ctx: AnnotationContext,
  store: AnnotationStore,
  id: string,
  targetId: string,
  watchedAt?: number,
): Promise<SourceDraft> {
  const target = ctx.session.state
    .get()
    .targets.find((t) => t.target_id === targetId);
  if (!target || target.capture.state !== "available")
    throw new SourceError("This target can’t be captured right now.");
  const receipt = await ctx.session.command(
    { command: "capture_target", target_id: targetId },
    { label: `Capture ${target.label}` },
  );
  const captured = await commandResult(ctx.session, receipt);
  if (captured?.kind !== "artifact_captured")
    throw new SourceError("The computer hasn’t confirmed a capture.");
  const a = captured.artifact;
  const provenance = captureProvenance(a, targetId, target.geometry.revision);
  const result = await download(ctx, {
    source: { kind: "artifact", artifact_id: a.artifact_id },
    label: `Get capture of ${target.label}`,
  });
  if (result.sha256 !== a.sha256)
    throw new SourceError(
      "The capture’s fingerprint changed. Capture it again.",
    );
  return importSource(
    store,
    id,
    {
      blob: result.blob,
      mediaType: a.media_type,
      capturedAt: provenance.captureGeometry.captured_at,
      origin: provenance.origin,
    },
    { artifactId: a.artifact_id, watchedAt, ...provenance },
  );
}
export function previewCaptureWords(origin: SourceOrigin): string | null {
  return origin.kind === "preview_capture"
    ? `Taken from the ${origin.preview_label} preview on ${origin.computer_name} · ${origin.captured_at === null ? "Capture time was not recorded" : new Date(origin.captured_at).toLocaleString()}`
    : null;
}
export function originWords(s: SourceDraft): string {
  const previewCapture = previewCaptureWords(s.origin);
  if (previewCapture) return previewCapture;
  if (s.origin.kind === "shown")
    return s.derivation?.kind === "frame_from_clip" ? "Frame from the agent’s video" : "Shown by the agent";
  if (s.derivation?.kind === "trim") return "Trimmed clip";
  if (s.derivation?.kind === "frame_from_clip") return "Frame from clip";
  if (s.derivation?.kind === "redact") return "Redacted image";
  if (s.derivation?.kind === "crop") return "Cropped image";
  const preview = s.origin.kind === "phone_import" ? previewName(s.origin.original_name) : null;
  if (preview) return `From preview · ${preview}`;
  return s.origin.kind === "phone_import"
    ? "From this device"
    : s.origin.kind === "clip"
      ? "Imported clip"
      : "Captured on the computer";
}
/** Retained computer previews are imported as `Preview: <label> · <time>.png`. */
function previewName(name: string | null | undefined): string | null {
  const match = name?.match(/^Preview: (.+) · [^·]+\.png$/);
  return match?.[1] ?? null;
}
export function validRegion(r: NormRect): boolean {
  return (
    [r.x, r.y, r.w, r.h].every(Number.isFinite) &&
    r.x >= 0 &&
    r.y >= 0 &&
    r.w > 0 &&
    r.h > 0 &&
    r.x + r.w <= 1.000001 &&
    r.y + r.h <= 1.000001
  );
}
/** Opaque pixel replacement, independent of theme, before PNG encoding. */
export function redactPixels(
  data: Uint8ClampedArray,
  width: number,
  height: number,
  regions: NormRect[],
): void {
  for (const r of regions) {
    if (!validRegion(r))
      throw new SourceError("Choose a rectangle inside the image.");
    for (
      let y = Math.floor(r.y * height);
      y < Math.ceil((r.y + r.h) * height);
      y++
    ) {
      for (
        let x = Math.floor(r.x * width);
        x < Math.ceil((r.x + r.w) * width);
        x++
      ) {
        const i = (y * width + x) * 4;
        data[i] = data[i + 1] = data[i + 2] = 0;
        data[i + 3] = 255;
      }
    }
  }
}
export async function deriveStill(
  store: AnnotationStore,
  id: string,
  source: SourceDraft,
  derivation: Extract<Derivation, { kind: "crop" | "redact" }>,
): Promise<SourceDraft> {
  if (!source.mediaType.startsWith("image/"))
    throw new SourceError("Choose a still image first.");
  const blob = source.blobKey ? await blobs.get(source.blobKey) : null;
  if (!blob)
    throw new SourceError("The original image is missing from this device.");
  const image = await createImageBitmap(blob),
    canvas = document.createElement("canvas");
  try {
    canvas.width = image.width;
    canvas.height = image.height;
    const context = canvas.getContext("2d", { willReadFrequently: true });
    if (!context)
      throw new SourceError("Image editing isn’t available on this device.");
    if (derivation.kind === "crop") {
      if (!validRegion(derivation))
        throw new SourceError("Choose a rectangle inside the image.");
      const x = Math.floor(derivation.x * image.width),
        y = Math.floor(derivation.y * image.height);
      canvas.width = Math.max(1, Math.ceil(derivation.w * image.width));
      canvas.height = Math.max(1, Math.ceil(derivation.h * image.height));
      context.drawImage(
        image,
        x,
        y,
        canvas.width,
        canvas.height,
        0,
        0,
        canvas.width,
        canvas.height,
      );
    } else {
      context.drawImage(image, 0, 0);
      const pixels = context.getImageData(0, 0, canvas.width, canvas.height);
      redactPixels(
        pixels.data,
        canvas.width,
        canvas.height,
        derivation.regions,
      );
      context.putImageData(pixels, 0, 0);
    }
    const derived = await importSource(
      store,
      id,
      {
        blob: await encodePng(canvas),
        mediaType: "image/png",
        origin: source.origin,
        capturedAt: source.capturedAt,
      },
      {
        derivedFrom: source.key,
        derivation,
        captureGeometry: source.captureGeometry,
        geometryChanged: source.geometryChanged,
        reproduction: source.reproduction,
      },
      source.key,
    );
    return derived;
  } finally {
    image.close();
  }
}
export function encodePng(canvas: HTMLCanvasElement): Promise<Blob> {
  return new Promise((resolve, reject) =>
    canvas.toBlob(
      (blob) =>
        blob
          ? resolve(blob)
          : reject(new SourceError("The image couldn’t be saved.")),
      "image/png",
    ),
  );
}
export function videoSize(
  blob: Blob,
): Promise<{ width: number; height: number; durationMs: number }> {
  return new Promise((resolve, reject) => {
    const video = document.createElement("video"),
      url = URL.createObjectURL(blob);
    const timer = setTimeout(
      () => finish(new SourceError("The clip took too long to open.")),
      15000,
    );
    const finish = (error?: Error) => {
      clearTimeout(timer);
      URL.revokeObjectURL(url);
      video.removeAttribute("src");
      video.load();
      if (error) reject(error);
    };
    video.preload = "metadata";
    video.onloadedmetadata = () => {
      if (!Number.isFinite(video.duration) || !video.videoWidth) {
        finish(
          new SourceError(
            "This clip’s duration or dimensions aren’t available.",
          ),
        );
        return;
      }
      resolve({
        width: video.videoWidth,
        height: video.videoHeight,
        durationMs: video.duration * 1000,
      });
      finish();
    };
    video.onerror = () =>
      finish(
        new SourceError(
          "This clip can’t be played on this device. Import screenshots instead.",
        ),
      );
    video.src = url;
  });
}
export async function extractMoment(
  store: AnnotationStore,
  id: string,
  source: SourceDraft,
  video: HTMLVideoElement,
  range?: ClipRange,
  quiet?: { wants(d: AnnotationDraft): boolean },
): Promise<SourceDraft> {
  if (video.readyState < 2 || video.seeking)
    throw new SourceError("Wait for this frame to load.");
  const parent = await verifiedSource(source);
  const at = Math.round(video.currentTime * 1000),
    canvas = document.createElement("canvas");
  if (range && !withinRange(at, range))
    throw new SourceError("This moment is outside the trim range.");
  canvas.width = video.videoWidth;
  canvas.height = video.videoHeight;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new SourceError("Frame extraction isn’t available.");
  ctx.drawImage(video, 0, 0);
  return importSource(
    store,
    id,
    {
      blob: await encodePng(canvas),
      mediaType: "image/png",
      // A frame of a video the agent showed says so; any other is from a clip.
      origin: source.origin.kind === "shown" ? source.origin : {
        kind: "clip",
        // The wire carries whole milliseconds (u64); media durations are fractional.
        duration_ms: Math.round(source.durationMs ?? 0),
        target_id: null,
      },
      capturedAt: source.capturedAt,
    },
    {
      derivedFrom: source.key,
      derivation: { kind: "frame_from_clip", at_ms: at },
      reproduction: {
        sourceKey: source.key,
        sourceSha256: source.sha256 ?? "",
        sourceSize: parent.size,
        requestedRangeMs: [
          Math.round((range?.start ?? 0) * 1000),
          Math.round((range?.end ?? (source.durationMs ?? 0) / 1000) * 1000),
        ],
        actualRangeMs: [
          Math.round((range?.start ?? 0) * 1000),
          Math.round((range?.end ?? (source.durationMs ?? 0) / 1000) * 1000),
        ],
        frameAtMs: at,
        method: "frame",
        silent: true,
      },
    },
    undefined,
    quiet,
  );
}

/** A still from `source` at `atMs`, seeked to in a video of its own so the
 * one being watched plays on. It stays through undo, like the words it
 * belongs to. */
export async function stillAt(
  store: AnnotationStore,
  id: string,
  source: SourceDraft,
  atMs: number,
  /** Whether a version of the draft has a comment waiting for this still. */
  wants: (d: AnnotationDraft) => boolean,
): Promise<SourceDraft> {
  const video = document.createElement("video"),
    url = URL.createObjectURL(await verifiedSource(source));
  try {
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new SourceError("This moment’s frame took too long to open.")),
        15000,
      );
      video.muted = true;
      video.playsInline = true;
      video.preload = "auto";
      video.onloadeddata = () => {
        video.currentTime = atMs / 1000;
      };
      video.onseeked = () => {
        clearTimeout(timer);
        resolve();
      };
      video.onerror = () => {
        clearTimeout(timer);
        reject(new SourceError("This video’s frame can’t be read on this device."));
      };
      video.src = url;
    });
    return await extractMoment(store, id, source, video, undefined, { wants });
  } finally {
    video.removeAttribute("src");
    video.load();
    URL.revokeObjectURL(url);
  }
}

/** Refuse old captures without provenance instead of attributing cached geometry. */
export function captureProvenance(
  artifact: ArtifactMeta,
  targetId: string,
  requestedRevision: number,
) {
  const capture = artifact.capture;
  if (!capture || capture.target_id !== targetId)
    throw new SourceError(
      `The computer didn’t return this capture’s geometry. Update ${PRODUCT_NAME} on the computer and capture again.`,
    );
  return {
    origin: {
      kind: "host_capture",
      target_id: capture.target_id,
      geometry_revision: capture.geometry.revision,
    } satisfies SourceOrigin,
    captureGeometry: capture,
    geometryChanged: capture.geometry.revision !== requestedRevision,
  };
}
