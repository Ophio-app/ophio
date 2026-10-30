// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import type { Route } from "../app/ui";
import { privateStepRoute } from "../screens/control/PrivateStepWarning";
import { Button } from "../design/Button";
import { Sheet } from "../design/Sheet";
import { Age } from "../design/Age";
import { useStore } from "../state/store";
import { useWarning } from "../app/warnings";
import type { Handoffs } from "./switching";
import "./lifecycle.css";

export function HandoffBanner({ handoffs, current, onReturn }: { handoffs: Handoffs; current: string; onReturn(id: string, route?: Route): void }) {
  const rows = useStore(handoffs, (s) => s);
  const [dismiss, setDismiss] = useState<string | null>(null);
  const warn = useWarning("control.dismiss");
  return <>{rows.filter((r) => r.profileId !== current || r.risk.privateTargets.length > 0).map((r) => {
    const name = handoffs.profiles.find((p) => p.profileId === r.profileId)?.name ?? "Previous computer";
    return <aside className="handoff-banner" key={r.profileId} aria-label={`Unresolved control on ${name}`}>
      <strong>{name} · Control not confirmed</strong>
      <div className="handoff-banner__actions">
        <Button variant="stop-outline" pending={r.busy} onClick={() => void handoffs.stop(r.profileId)}>Stop on {name}</Button>
        <Button disabled={r.busy} onClick={() => void handoffs.release(r.profileId)}>Retry release</Button>
      </div>
      <details><summary>Handoff details</summary><p>{r.detail}</p><p>Requested <Age at={r.since} suffix=" ago" /></p>
        <div className="handoff-banner__actions"><Button onClick={() => {
          const targetId = r.risk.privateTargets[0];
          const state = handoffs.sessions.get(r.profileId)?.state.get();
          onReturn(r.profileId, targetId ? privateStepRoute(state, targetId) : undefined);
        }}>Return to {name}</Button>
          <Button variant="quiet" onClick={() => warn.ask(() => setDismiss(r.profileId), () => handoffs.dismiss(r.profileId))}>Dismiss warning</Button></div>
      </details>
    </aside>;
  })}<Sheet open={dismiss !== null} title="Dismiss unresolved control?" onClose={() => setDismiss(null)} footer={<Button variant="warn" onClick={() => { if (dismiss) { warn.agreed(); handoffs.dismiss(dismiss); } setDismiss(null); }}>Dismiss without confirmation</Button>}>
    <p>The computer hasn’t confirmed that control or work stopped. This only removes the warning. Check that computer before assuming it is safe.</p>
    {warn.check}
  </Sheet></>;
}

/** A modal makes the banner inert; mirror its urgent action inside every sheet. */
export function HandoffStops({ handoffs, current }: { handoffs: Handoffs; current: string }) {
  const rows = useStore(handoffs, (s) => s);
  return <>{rows.filter((r) => r.profileId !== current).map((r) => {
    const name = handoffs.profiles.find((p) => p.profileId === r.profileId)?.name ?? "previous computer";
    return <div className="handoff-banner__actions" key={r.profileId}><Button variant="stop-outline" className="intervention" pending={r.busy} onClick={() => void handoffs.stop(r.profileId)}>Stop on {name}</Button></div>;
  })}</>;
}
