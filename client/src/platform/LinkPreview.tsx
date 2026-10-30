// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from "react";
import { Sheet } from "../design/Sheet";
import { Button } from "../design/Button";
import { Notice } from "../design/Notice";
import { HostSession } from "../host/session";
import type { HostProfile, Platform } from "../host/link";
import type { Route } from "../app/ui";
import { pairedComputer, resolveObject, type ObjectLink, type LinkResolution } from "./links";
import "./lifecycle.css";

export function waitForConnection(session: HostSession, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const finish = (error?: Error) => { clearTimeout(timer); off(); signal.removeEventListener("abort", abort); error ? reject(error) : resolve(); };
    const check = () => {
      const state = session.link.get();
      if (state.state === "online") finish();
      else if (state.state === "failed") finish(new Error(state.detail));
    };
    const off = session.link.subscribe(check);
    const timer = setTimeout(() => finish(new Error("The computer hasn’t answered. Reconnect and check the link again.")), 8000);
    const abort = () => finish(new Error("Link preview closed."));
    signal.addEventListener("abort", abort, { once: true });
    if (signal.aborted) abort(); else check();
  });
}
export function LinkPreview({ link, platform, profiles, existingSession, onClose, onOpen }: {
  existingSession?: HostSession; link: ObjectLink; platform: Platform; profiles: HostProfile[]; onClose(): void;
  onOpen(profile: HostProfile, route: Route, session: HostSession): void;
}) {
  const preview = useRef<{ session: HostSession; transferred: boolean } | null>(null);
  const profile = pairedComputer(link, profiles, existingSession?.profileId);
  const [result, setResult] = useState<LinkResolution | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    if (!profile) return;
    const shared = existingSession?.profileId === profile.profileId;
    const session = shared ? existingSession : new HostSession(profile.profileId, platform.control(profile.profileId));
    const owner = { session, transferred: false };
    preview.current = owner;
    const cancel = new AbortController();
    setResult(null); setError(null);
    if (!shared) session.start();
    void waitForConnection(session, cancel.signal).then(() => resolveObject(link, session)).then(
      (value) => { if (!cancel.signal.aborted) setResult(value); },
      (e: unknown) => { if (!cancel.signal.aborted) setError(e instanceof Error ? e.message : "The computer couldn’t check this item."); },
    );
    return () => { cancel.abort(); if (!shared && !owner.transferred) session.stop(); };
  }, [link, profile, platform, existingSession, attempt]);
  const route = result && "route" in result ? result.route : null;
  const open = useRef<() => void>(() => {});
  open.current = () => { const owner = preview.current; if (!owner || !route || !profile || owner.transferred) return; owner.transferred = true; onOpen(profile, route, owner.session); };
  return <Sheet open title="Open link" onClose={onClose} footer={route && profile && <Button variant="primary" onClick={() => open.current()}>Open item</Button>}>
    <div className="link-preview">
      {!profile ? <Notice title="Computer not paired">This device isn’t paired with the computer named by this link.</Notice>
        : <>{!result && !error && <p role="status">Checking access with {profile.name}…</p>}
          {error && <Notice tone="unknown" action={<Button onClick={() => setAttempt((n) => n + 1)}>Try again</Button>}>{error}</Notice>}
          {result && ("label" in result ? <><p>Open {result.label} on {profile.name}?</p>{result.state === "settled" && <Notice>{result.detail}</Notice>}<p className="muted">Opening an item doesn’t answer it or start work.</p></> : <Notice tone="warn">{result.detail}</Notice>)}
        </>}
    </div>
  </Sheet>;
}
