// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, type ReactNode } from "react";
import { useApp, useHost, useLink } from "../app/context";
import type { Route } from "../app/ui";
import { Button } from "../design/Button";
import { Age } from "../design/Age";
import { Notice } from "../design/Notice";
import { Screen } from "./Screen";
import "./offline.css";

export function lastReached(profileId: string): number | null {
  try { const value = Number(localStorage.getItem(`last-reached.${profileId}`)); return Number.isFinite(value) && value > 0 ? value : null; } catch { return null; }
}
export function ColdStartOffline({ route, children }: { route: Route; children: ReactNode }) {
  const { profile, session, ui } = useApp();
  const link = useLink(), host = useHost((s) => s.host);
  const attempts = useRef(0);
  if (link.state === "connecting") attempts.current = link.attempt;
  // Read from the store rather than rendered: it moves with every streamed
  // word, and every mounted screen has this wrapper recording the same value.
  useEffect(() => {
    const remember = () => {
      const heard = session.state.get().lastHeardAt;
      const key = `last-reached.${profile.profileId}`;
      if (heard) try { if (localStorage.getItem(key) !== String(heard)) localStorage.setItem(key, String(heard)); } catch { /* Still show current observations. */ }
    };
    remember();
    return session.state.subscribe(remember);
  }, [session, profile.profileId]);
  if (host || !["talk", "work", "control", "todo"].includes(route.screen) || link.state === "online") return children;
  const last = lastReached(profile.profileId);
  const incompatible = link.state === "failed" && link.reason === "incompatible_version";
  return <Screen title="Connection" footer={<div className="offline-foot"><Button variant="primary" block onClick={() => session.reconnect()}>Retry connection</Button></div>}><div className="offline-card">
    <h1>{incompatible ? "An update is needed" : `Can’t reach ${profile.name}`}</h1>
    <p>{attempts.current ? `Still trying, attempt ${attempts.current}.` : "Connecting…"}</p>
    {!incompatible && <p>Check that {profile.name} is switched on and not asleep, that Øphio is running on it, and that this phone is online. It keeps trying by itself.</p>}
    <p className="muted">{last ? <>Last reached <Age at={last} suffix=" ago" /></> : "This device hasn’t recorded a successful connection yet."}</p>
    {link.state === "offline" && <Notice tone="warn">{link.reason}</Notice>}
    {link.state === "failed" && <Notice tone="warn">{link.detail}</Notice>}
    <p>Drafts, Inbox items and downloaded files stay on your device. Reading task content needs a copy already cached on this device.</p>
    <div className="offline-card__actions"><Button onClick={() => ui.push({ screen: "inbox" }, "work")}>Open Inbox</Button><Button onClick={() => ui.push({ screen: "settings", section: incompatible ? "settings-versions" : undefined })}>Settings</Button></div>
    <p className="mono offline-card__address">{profile.addresses.join(", ")}</p>
    <details><summary>How this phone recognizes {profile.name}</summary><dl><dt>Computer</dt><dd className="mono">{profile.hostId}</dd><dt>Address</dt><dd className="mono">{profile.addresses.join(", ")}</dd><dt>Certificate</dt><dd className="mono">{profile.certSha256.toUpperCase().match(/.{1,4}/g)?.join(" ")}</dd></dl></details>
  </div></Screen>;
}
