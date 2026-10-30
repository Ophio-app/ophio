// SPDX-License-Identifier: Apache-2.0
import { sha256Hex } from "../../host/transfer";
import { blobs, type SourceDraft } from "../../state/annotations";

export interface DerivativeIdentity {
  sourceKey: string;
  sourceSha256: string;
  sourceSize: number;
  requestedRangeMs: [number, number];
  actualRangeMs: [number, number];
  method: "android-remux" | "webcodecs" | "frame";
  /** Exported clips deliberately omit every audio track. */
  silent: true;
  frameAtMs?: number;
}

export class DerivativeError extends Error {
  override name = "DerivativeError";
}

/** Read and hash once, then pass this immutable Blob to the eventual writer. */
export async function verifiedSource(source: SourceDraft): Promise<Blob> {
  const blob = source.blobKey ? await blobs.get(source.blobKey) : null;
  if (!blob)
    throw new DerivativeError(
      `${source.key} is missing from this device. Import it again.`,
    );
  if (
    !source.sha256 ||
    (source.byteSize !== undefined && source.byteSize !== blob.size) ||
    (await sha256Hex(blob)) !== source.sha256
  )
    throw new DerivativeError(
      `${source.key} changed since review. Nothing was sent. Import it again and review the new bytes.`,
    );
  return blob;
}

/** Validate the entire outgoing set before the first upload is declared. */
export async function verifyOutgoing(
  sources: SourceDraft[],
): Promise<Map<string, Blob>> {
  const result = new Map<string, Blob>();
  for (const source of sources) {
    if (source.blobKey) result.set(source.key, await verifiedSource(source));
    else if (!source.artifactId || source.reproduction)
      throw new DerivativeError(
        `${source.key} is missing from this device. Import it again.`,
      );
  }
  return result;
}

export const clipTime = (ms: number): string => {
  // Round to the shown hundredths first, so 59.997 s reads 1:00.00, not 0:60.00.
  const hundredths = Math.round(ms / 10);
  const minutes = Math.floor(hundredths / 6000);
  return `${minutes}:${((hundredths - minutes * 6000) / 100).toFixed(2).padStart(5, "0")}`;
};
export const clipSize = (bytes: number): string =>
  bytes < 1024 * 1024
    ? `${(bytes / 1024).toFixed(1)} KB`
    : `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
