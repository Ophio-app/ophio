// SPDX-License-Identifier: Apache-2.0
//
// Handoffs from Clankerhub waiting on this phone, at the top of Tasks
// (CH-016 D-7): each until it's opened, dismissed or expires, and nothing
// while none waits. They belong to no project yet, so they're the phone's own
// rows, not the computer's alerts. Scanning a code is one of New's kinds.

import { Camera, Gamepad2 } from "lucide-react";
import { useEffect, useState } from "react";
import { acceptObjectLink } from "../../platform/links";
import { useApp } from "../../app/context";
import { ListRow, Section } from "../../design/ListRow";
import { clockTime } from "../../design/time";
import { toast } from "../../design/Toast";
import { useStore } from "../../state/store";
import { blockerFor, stageOf } from "./codes";
import { expiry, handoffs, type Handoff } from "./handoffs";
import { SaveWarning } from "./SaveWarning";

const PRUNE_MS = 60_000;

export function WaitingHandoffs() {
  const { ui, profile } = useApp();
  const store = handoffs();
  const all = useStore(store, (s) => s);
  useEffect(() => {
    const prune = () => {
      const before = store.expiredRemoved;
      store.prune();
      if (store.expiredRemoved > before) toast("Expired handoffs are removed. Nothing was downloaded.");
    };
    prune();
    const timer = setInterval(prune, PRUNE_MS);
    return () => clearInterval(timer);
  }, [store]);
  if (!all.length) return null;
  return (
    <Section title="From Clankerhub">
      <SaveWarning />
      <div className="group">
        {all.map((h) => (
          <ListRow key={h.id} icon={Gamepad2} chevron title={h.gameId ? <span className="mono">{h.gameId}</span> : "A game from Clankerhub"}
            subtitle={status(h, profile.name)} ariaLabel={`Open ${h.gameId ?? "a game"} from Clankerhub`}
            onClick={() => ui.push({ screen: "contribution", handoffId: h.id }, "work")} />
        ))}
      </div>
    </Section>
  );
}

/** New's way in for a handoff, on a phone with a camera to scan a code. */
export function ScanClankerhub({ before }: { before?: () => void }) {
  const { platform } = useApp();
  const [scanning, setScanning] = useState(false);
  if (!platform.scanPairingCode) return null;
  return (
    <ListRow icon={Camera} title="Scan a Clankerhub code" ariaLabel="Scan a Clankerhub code" chevron disabled={scanning}
      subtitle="Open a game someone handed off to this phone"
      onClick={async () => {
        before?.();
        setScanning(true);
        try { const link = await platform.scanPairingCode?.(); if (link) acceptObjectLink(link); }
        catch { toast("The camera couldn’t scan. Try again."); }
        finally { setScanning(false); }
      }} />
  );
}

/** Where a waiting handoff is, in a few words. */
export function status(h: Handoff, computer: string): string {
  const until = `works until ${clockTime(expiry(h))}`;
  if (h.unsettled) return `Asking ${computer} whether it started`;
  if (!h.view) return `Not opened yet · ${until}`;
  switch (stageOf(h.view)) {
    case "review": return `Waiting for you · ${until}`;
    case "progress": return "Getting the source";
    case "ready": return "Ready · nothing has run yet";
    case "canceled": return "Canceled";
    case "reconcile": return "Checking what finished";
    case "blocked": return blockerFor(h.view.code, { computer, folder: null, started: h.started || h.unsettled !== null }).heading;
  }
}
