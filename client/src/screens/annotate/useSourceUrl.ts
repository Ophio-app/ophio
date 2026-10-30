// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { type SourceDraft } from "../../state/annotations";
import { verifiedSource } from "./derivatives";
export function useSourceUrl(source: SourceDraft | undefined, supplied?: string): {
  url: string | null;
  error: string | null;
} {
  const [state, setState] = useState<{
    key: string | null;
    url: string | null;
    error: string | null;
  }>({ key: null, url: null, error: null });
  const key = source?.blobKey ?? null;
  useEffect(() => {
    let cancelled = false,
      url: string | null = null;
    if (supplied) return;
    if (!key || !source) {
      setState({
        key,
        url: null,
        error: "This image isn’t stored on this device.",
      });
      return;
    }
    void verifiedSource(source)
      .then((blob) => {
        if (cancelled) return;
        if (!blob) {
          setState({
            key,
            url: null,
            error: "This source is missing from this device’s storage.",
          });
          return;
        }
        url = URL.createObjectURL(blob);
        setState({ key, url, error: null });
      })
      .catch((error: unknown) => {
        if (!cancelled)
          setState({
            key,
            url: null,
            error:
              error instanceof Error
                ? error.message
                : "Device storage couldn’t be opened.",
          });
      });
    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [key, source, supplied]);
  if (supplied) return { url: supplied, error: null };
  return state.key === key ? state : { url: null, error: null };
}
