// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { useApp, useLink } from "../../app/context";
import { download } from "../../host/transfer";
import { CommandError, type HostSession } from "../../host/session";
import type { Platform } from "../../host/link";

type Context = { platform: Platform; session: HostSession; profileId: string };
/** An image or video the computer keeps; a video's size is 0 × 0 until played. */
export interface Picture {
  url: string;
  blob: Blob;
  sha256: string;
  width: number;
  height: number;
}
interface Cached {
  promise: Promise<Picture>;
  picture?: Picture;
  users: number;
  touched: number;
}
const cache = new Map<string, Cached>();
const MAX_BYTES = 24 * 1024 * 1024;
function trim() {
  let bytes = [...cache.values()].reduce(
    (n, c) => n + (c.picture?.blob.size ?? 0),
    0,
  );
  for (const [key, c] of [...cache.entries()].sort(
    (a, b) => a[1].touched - b[1].touched,
  )) {
    if (cache.size <= 8 && bytes <= MAX_BYTES) break;
    if (!c.users && c.picture) {
      URL.revokeObjectURL(c.picture.url);
      bytes -= c.picture.blob.size;
      cache.delete(key);
    }
  }
}
export function imageSize(
  blob: Blob,
): Promise<{ width: number; height: number }> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(blob);
    const img = new Image();
    img.onload = () => {
      URL.revokeObjectURL(url);
      resolve({ width: img.naturalWidth, height: img.naturalHeight });
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error("This image couldn’t be opened."));
    };
    img.src = url;
  });
}
export function acquirePicture(ctx: Context, id: string, expectedSha?: string) {
  const key = `${ctx.profileId}/${id}/${expectedSha ?? ""}`;
  let entry = cache.get(key);
  if (!entry) {
    const promise = (async () => {
      const metadata = await ctx.session.query({
        kind: "artifact",
        artifact_id: id,
      });
      if (metadata.artifact.size > MAX_BYTES)
        throw new Error(
          "This picture is too large for the receipt preview. Open it through Transfers.",
        );
      const result = await download(ctx, {
        source: { kind: "artifact", artifact_id: id },
        label: "Load receipt picture",
        shownInTask: true,
      });
      if (
        result.sha256 !== metadata.artifact.sha256 ||
        (expectedSha && result.sha256 !== expectedSha)
      )
        throw new Error(
          "The picture’s content doesn’t match the original evidence.",
        );
      const blob = new Blob([result.blob], {
        type: metadata.artifact.media_type,
      });
      // A video opens in its own player, which reports its size.
      const size = metadata.artifact.media_type.startsWith("video/")
        ? { width: 0, height: 0 }
        : await imageSize(blob);
      const picture = {
        ...size,
        blob,
        sha256: result.sha256,
        url: URL.createObjectURL(blob),
      };
      return picture;
    })().catch((e: unknown) => {
      cache.delete(key);
      throw e;
    });
    const fresh: Cached = { users: 0, touched: Date.now(), promise };
    entry = fresh;
    cache.set(key, fresh);
    void promise.then(
      (picture) => {
        fresh.picture = picture;
        trim();
      },
      () => undefined,
    );
  }
  const owned = entry;
  owned.users++;
  owned.touched = Date.now();
  return {
    promise: owned.promise,
    release: () => {
      owned.users = Math.max(0, owned.users - 1);
      trim();
    },
  };
}
export function usePicture(id: string | undefined, sha?: string) {
  const { platform, profile, session } = useApp();
  const link = useLink();
  const [result, setResult] = useState<{
    id?: string;
    picture?: Picture;
    error?: string;
    /** Expired from the computer: trying again won't bring it back. */
    gone?: boolean;
  }>({});
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    if (!id) return;
    let alive = true;
    setResult({ id });
    const handle = acquirePicture(
      { platform, session, profileId: profile.profileId },
      id,
      sha,
    );
    void handle.promise
      .then((picture) => {
        if (alive) setResult({ id, picture });
      })
      .catch((e: unknown) => {
        if (alive)
          setResult({
            id,
            gone: e instanceof CommandError && e.error.code === "expired",
            error:
              e instanceof Error
                ? e.message
                : "The computer couldn’t provide this picture.",
          });
      });
    return () => {
      alive = false;
      handle.release();
    };
  }, [id, sha, platform, profile.profileId, session, retry, link.state]);
  return {
    ...(result.id === id ? result : {}),
    retry: () => setRetry((n) => n + 1),
  };
}
