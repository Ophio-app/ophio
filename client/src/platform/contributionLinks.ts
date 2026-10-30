// SPDX-License-Identifier: Apache-2.0
// Untrusted transport data only. The paired computer checks the referenced
// record; the existing receiver separately decides compatibility and trust.
import { Store } from "../state/store";

export const CONTRIBUTION_LINK_PREFIX = "ophio-contribution://open/v1?";
export const CONTRIBUTION_DELIVERY_FEATURE = "contribution_delivery_v1";
export const CONTRIBUTION_LINK_LIMIT = 96 * 1024;
const KEY = "contribution-delivery.v1";
const bytes = new TextEncoder();
export interface ContributionLink { url: string; packetJson: string; handoffUrl: string; expiresAt: number }

export function parseContributionLink(url: string): ContributionLink | null {
  if (!url.startsWith(CONTRIBUTION_LINK_PREFIX) || url.length > CONTRIBUTION_LINK_LIMIT || /[#\s]/.test(url)) return null;
  try {
    const query = new URLSearchParams(url.slice(CONTRIBUTION_LINK_PREFIX.length));
    const encoded = query.get("packet"), handoffUrl = query.get("handoff");
    if ([...query].length !== 2 || !encoded || !/^[A-Za-z0-9_-]+$/.test(encoded) || !handoffUrl) return null;
    const raw = atob(encoded.replaceAll("-", "+").replaceAll("_", "/"));
    if (raw.length > 65_536 || btoa(raw).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "") !== encoded) return null;
    const packetJson = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(Uint8Array.from(raw, c => c.charCodeAt(0)));
    const p: unknown = JSON.parse(packetJson);
    if (!p || typeof p !== "object" || Array.isArray(p)) return null;
    const packet = p as Record<string, unknown>;
    if (Object.keys(packet).length !== 6 || packet.schemaVersion !== 1 || packet.kind !== "contribution-import"
      || typeof packet.packetId !== "string" || !/^[a-z0-9][a-z0-9._-]{0,79}$/.test(packet.packetId)
      || typeof packet.handoffSha256 !== "string" || !/^[a-f0-9]{64}$/.test(packet.handoffSha256)
      || typeof packet.issuedAt !== "number" || !Number.isSafeInteger(packet.issuedAt) || packet.issuedAt < 0
      || typeof packet.expiresAt !== "number" || !Number.isSafeInteger(packet.expiresAt)
      || packet.expiresAt <= packet.issuedAt || packet.expiresAt - packet.issuedAt > 600) return null;
    // Match the receiver's narrow canonical public-HTTPS profile. DNS and
    // private-address rejection are authoritative on the computer.
    const u = new URL(handoffUrl);
    if (handoffUrl.length > 2048 || u.href !== handoffUrl || u.protocol !== "https:" || u.port || u.username || u.password || u.search || u.hash
      || !/^[a-z0-9.-]+$/.test(u.hostname) || !u.hostname.includes(".") || u.hostname.endsWith(".")
      || u.hostname.split(".").some(p => !p || p.startsWith("-") || p.endsWith("-"))
      || /^(?:[0-9]+\.){3}[0-9]+$/.test(u.hostname)
      || /(?:^|\.)(?:localhost|local|internal|test|invalid|onion)$/.test(u.hostname)
      || !/^\/(?:[A-Za-z0-9._-]+\/)*[A-Za-z0-9._-]+$/.test(u.pathname)) return null;
    return { url, packetJson, handoffUrl, expiresAt: packet.expiresAt };
  } catch { return null; }
}

/** Producer serializes its packet once, then carries that exact string. */
export function contributionLink(packetJson: string, handoffUrl: string): string {
  const encoded = btoa(Array.from(bytes.encode(packetJson), b => String.fromCharCode(b)).join(""))
    .replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
  const url = `${CONTRIBUTION_LINK_PREFIX}packet=${encoded}&handoff=${encodeURIComponent(handoffUrl)}`;
  if (!parseContributionLink(url)) throw new Error("Invalid contribution link");
  return url;
}

function load(): ContributionLink | null {
  try { return parseContributionLink(localStorage.getItem(KEY) ?? ""); } catch { return null; }
}
// One pending launch, bounded before persistence. It survives pairing and app
// restart, but its expiry never extends; Resume must create a new packet.
export class ContributionInbox extends Store<ContributionLink | null> {
  constructor() { super(load()); }
  receive(value: ContributionLink | null): void {
    this.set(value);
    try { if (value) localStorage.setItem(KEY, value.url); else localStorage.removeItem(KEY); } catch { /* Retain for this process. */ }
  }
}
