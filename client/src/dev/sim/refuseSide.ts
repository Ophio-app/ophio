// SPDX-License-Identifier: Apache-2.0
import type { ErrorCode } from "../../protocol/types";
import type { LinkState, MediaLink, SpeechLink, TerminalLink, TransferLink } from "../../host/link";

export type SideKind = "terminal" | "media" | "speech" | "transfer";
export type SideRefusal = { code: ErrorCode; reason: string };

/** A fatal frame ends only this connection, after observable connection progress. */
export function refusedSide(refusal: SideRefusal): TerminalLink & MediaLink & SpeechLink & TransferLink {
  type StateEvent = { type: "state"; state: LinkState };
  const listeners = new Set<(event: StateEvent) => void>();
  let state: LinkState = { state: "connecting", attempt: 1 };
  let closed = false;
  const timer = setTimeout(() => {
    if (closed) return;
    state = { state: "offline", since: Date.now(), code: refusal.code, reason: refusal.reason, retryAt: null };
    for (const listener of listeners) listener({ type: "state", state });
  }, 200);
  return {
    send: () => false, input: () => false, chunk: async () => false,
    close() { closed = true; clearTimeout(timer); listeners.clear(); },
    listen(listener) {
      listeners.add(listener);
      queueMicrotask(() => { if (!closed && listeners.has(listener)) listener({ type: "state", state }); });
      return () => { listeners.delete(listener); };
    },
  };
}
