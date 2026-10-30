// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { useApp, useHost, useLink } from "../app/context";
import { Button } from "../design/Button";
import { ListRow, Section } from "../design/ListRow";
import { Sheet } from "../design/Sheet";
import type { HostSession } from "../host/session";
import type { QueryData } from "../protocol/types";
import { useStore } from "../state/store";
import { handoffs } from "../screens/contribute/handoffs";
import { CONTRIBUTION_DELIVERY_FEATURE, type ContributionInbox, type ContributionLink } from "./contributionLinks";
import { appLock } from "./lock";

/** Resolves only while unlocked and on an authenticated paired connection.
 * A stale reply cannot navigate after locking, switching hosts or a new link. */
export function ContributionDelivery({ inbox, locked }: { inbox: ContributionInbox; locked: boolean }) {
  const { session, ui, platform, profile, profiles, switchTo } = useApp();
  const pending = useStore(inbox, s => s);
  const online = useLink().state === "online";
  const capable = useHost(s => s.capabilities?.features.includes(CONTRIBUTION_DELIVERY_FEATURE) ?? false);
  const manage = useHost(s => s.grants?.admin === true);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    setError(null);
    if (!pending || locked || !online || !capable || !manage) return;
    if (Date.now() / 1000 >= pending.expiresAt) { setError("This handoff expired. Open it again in Clankerhub to get a fresh one."); return; }
    let live = true;
    const current = () => live && inbox.get() === pending && !appLock(platform).get().locked && !appLock(platform).get().checking
      && session.link.get().state === "online" && session.state.get().grants?.admin === true
      && !!session.state.get().capabilities?.features.includes(CONTRIBUTION_DELIVERY_FEATURE);
    void readDelivery(session, pending, current).then(result => {
      if (!result || !current()) return;
      if (Date.now() / 1000 >= pending.expiresAt) throw new Error("Expired handoff");
      const handoff = handoffs().receive(pending.packetJson, result.handoff_json);
      if (!handoff) throw new Error("Record too large");
      inbox.receive(null);
      ui.push({ screen: "contribution", handoffId: handoff.id }, "work");
    }).catch(() => { if (current()) setError("The computer couldn’t read this handoff. Try again, or open a fresh handoff in Clankerhub."); });
    return () => { live = false; };
  }, [pending, locked, online, capable, manage, session, ui, platform, inbox, retry]);
  const problem = !online ? "Connect to a paired computer to open this handoff."
    : !capable ? "Update Øphio on this computer to receive Clankerhub handoffs."
    : !manage ? "This device needs Manage permission on the computer to receive a handoff." : error;
  return <Sheet open={!!pending && !locked} title="Opening from Clankerhub" subtitle={profile.name} onClose={() => inbox.receive(null)}
    footer={<>{problem && <Button onClick={() => setRetry(n => n + 1)}>Try again</Button>}<Button variant="quiet" onClick={() => inbox.receive(null)}>Dismiss</Button></>}>
    <p role="status">{problem ?? "Reading the handoff on your paired computer…"}</p>
    {profiles.length > 1 && <Section title="Use another computer"><div className="group">
      {profiles.filter(p => p.profileId !== profile.profileId).map(p => <ListRow key={p.profileId}
        title={`Switch to ${p.name}`} ariaLabel={`Switch to ${p.name}`} subtitle={p.addresses.join(", ")}
        onClick={() => switchTo(p.profileId)} />)}
    </div></Section>}
  </Sheet>;
}

type Delivery = Extract<QueryData, { kind: "contribution_delivery" }>;
const reads = new WeakMap<HostSession, { link: ContributionLink; result: Promise<Delivery> }>();

/** Effect cleanup cannot cancel a host fetch. Keep at most one delivery read
 * in flight per session, sharing it across repeated opens and lock/unlock.
 * Different links wait before sending so their query timeout excludes this
 * fetch. Recheck the waiting effect before sending; only the latest may read. */
async function readDelivery(session: HostSession, link: ContributionLink, current: () => boolean): Promise<Delivery | null> {
  while (current()) {
    const active = reads.get(session);
    if (active) {
      if (active.link.packetJson === link.packetJson && active.link.handoffUrl === link.handoffUrl) return active.result;
      await active.result.catch(() => undefined);
      continue;
    }
    if (Date.now() / 1000 >= link.expiresAt) throw new Error("Expired handoff");
    const result = session.query({ kind: "contribution_delivery", packet_json: link.packetJson, handoff_url: link.handoffUrl })
      .finally(() => reads.delete(session));
    reads.set(session, { link, result });
    return result;
  }
  return null;
}
