// SPDX-License-Identifier: Apache-2.0
import { sha256Hex, upload, TransferError } from "../../host/transfer";
import {
  blobs,
  type AnnotationStore,
  type SourceDraft,
} from "../../state/annotations";
import { createClip } from "./clipEncoding";
import {
  DerivativeError,
  verifiedSource,
  type DerivativeIdentity,
} from "./derivatives";
import { videoSize, type AnnotationContext } from "./sources";
import { applyClipTrim, type ClipRange } from "./trim";

/** Publish all derivatives together only after encode, decode and storage succeed. */
export async function prepareReproduction(
  store: AnnotationStore,
  id: string,
  source: SourceDraft,
  range: ClipRange,
  signal: AbortSignal,
): Promise<void> {
  const input = await verifiedSource(source);
  const output = await createClip(input, range, signal);
  const dimensions = await videoSize(output.blob);
  if (
    Math.abs(
      dimensions.durationMs - (output.range.end - output.range.start) * 1000,
    ) > 10
  )
    throw new DerivativeError(
      "The trimmed clip’s duration could not be verified. Use screenshots instead.",
    );
  const identity: DerivativeIdentity = {
    sourceKey: source.key,
    sourceSha256: source.sha256 ?? "",
    sourceSize: input.size,
    requestedRangeMs: [
      Math.round(range.start * 1000),
      Math.round(range.end * 1000),
    ],
    actualRangeMs: [
      Math.round(output.range.start * 1000),
      Math.round(output.range.end * 1000),
    ],
    method: output.method,
    silent: true,
  };
  const saved: Omit<SourceDraft, "key">[] = [];
  const keys: string[] = [];
  try {
    for (const [index, part] of [
      { blob: output.blob, atMs: null },
      ...output.frames,
    ].entries()) {
      signal.throwIfAborted();
      const sha256 = await sha256Hex(part.blob);
      const blobKey = await blobs.put(part.blob);
      keys.push(blobKey);
      saved.push({
        blobKey,
        artifactId: null,
        sha256,
        byteSize: part.blob.size,
        mediaType: part.blob.type,
        width: dimensions.width,
        height: dimensions.height,
        durationMs: index === 0 ? dimensions.durationMs : null,
        capturedAt: source.capturedAt,
        origin: {
          kind: "clip",
          duration_ms: Math.round(source.durationMs ?? 0),
          target_id:
            source.origin.kind === "clip" ? source.origin.target_id : null,
        },
        derivedFrom: source.key,
        derivation:
          part.atMs === null
            ? {
                kind: "trim",
                start_ms: identity.actualRangeMs[0],
                end_ms: identity.actualRangeMs[1],
              }
            : { kind: "frame_from_clip", at_ms: part.atMs },
        included: index !== 0,
        reproduction: {
          ...identity,
          ...(part.atMs === null
            ? {}
            : { method: "frame", frameAtMs: part.atMs }),
        },
      });
    }
    signal.throwIfAborted();
    const current = store.get()[id];
    const edits = current?.clipEdits?.[source.key];
    if (
      !current ||
      !current.sources.some(
        (s) => s.key === source.key && s.sha256 === source.sha256,
      ) ||
      (edits &&
        (Number(edits.start) !== range.start ||
          Number(edits.end) !== range.end))
    )
      throw new DerivativeError(
        "The selected range changed. Create the clip again from the new range.",
      );
    store.update(id, (d) => {
      const trimmed = applyClipTrim(d, source.key, output.range);
      const added = saved.map((s, i) => ({
        ...s,
        key: `S${d.nextSource + i}`,
      }));
      return {
        ...trimmed,
        sources: [...trimmed.sources, ...added],
        nextSource: d.nextSource + added.length,
        selectedSource: added[1]?.key ?? source.key,
        clipEdits: {
          ...d.clipEdits,
          [source.key]: {
            start: String(output.range.start),
            end: String(output.range.end),
            at: output.range.start,
          },
        },
      };
    });
    if (store.persistence.get()) {
      store.record(id, () => current);
      throw new DerivativeError(
        "The clip’s draft could not be saved. Free some device storage and try again. Your earlier draft is kept.",
      );
    }
  } catch (error) {
    await Promise.all(keys.map((key) => blobs.remove(key)));
    throw error;
  }
}

export async function sendReviewedClip(
  ctx: AnnotationContext,
  store: AnnotationStore,
  id: string,
  source: SourceDraft,
): Promise<void> {
  if (source.derivation?.kind !== "trim" || !source.reproduction)
    throw new DerivativeError("Create and review a trimmed clip first.");
  // No command or transfer can leave the phone until this exact Blob verifies.
  const blob = await verifiedSource(source);
  if (source.clipTransfer?.artifactId) return;
  const result = await upload(ctx, {
    blob,
    sha256: source.sha256 ?? undefined,
    name: `reproduction-${source.key}.${source.mediaType === "video/mp4" ? "mp4" : "webm"}`,
    label: "Send reviewed clip to computer",
    destination: { kind: "artifact" },
    transferId: source.clipTransfer?.transferId,
    onDeclared: (transferId) =>
      store.record(id, (d) => ({
        ...d,
        sources: d.sources.map((s) =>
          s.key === source.key
            ? { ...s, clipTransfer: { transferId, artifactId: null } }
            : s,
        ),
      })),
  }).catch((error: unknown) => {
    if (error instanceof TransferError && !error.resumable)
      store.record(id, (d) => ({
        ...d,
        sources: d.sources.map((s) =>
          s.key === source.key ? { ...s, clipTransfer: undefined } : s,
        ),
      }));
    throw error;
  });
  if (!result.artifactId)
    throw new DerivativeError(
      "The computer hasn’t confirmed the clip. Check Transfers before trying again.",
    );
  store.record(id, (d) => ({
    ...d,
    sources: d.sources.map((s) =>
      s.key === source.key && s.sha256 === source.sha256
        ? {
            ...s,
            clipTransfer: {
              transferId: result.transferId,
              artifactId: result.artifactId,
            },
          }
        : s,
    ),
  }));
}
