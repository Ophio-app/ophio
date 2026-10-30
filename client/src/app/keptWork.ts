// SPDX-License-Identifier: Apache-2.0
//
// Everything this phone keeps for a computer (drafts, markup, Inbox items,
// files waiting to send) is stored under its pairing's profile id, a random
// UUID, and each pairing has a new one. Forgetting a computer keeps that work here and notes
// whose it was; pairing the same computer again moves it to the new pairing.

import type { HostProfile } from "../host/link";
import { previewDrafts } from "../screens/control/previewDrafts";
import { transferBytes } from "../screens/transfers/storage";
import { reportPlatformError } from "../platform/errors";

const KEY = "kept-work.v1";

interface Kept {
  profileId: string;
  hostId: string;
  at: number;
}

function read(): Kept[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(value)
      ? value.filter((k): k is Kept => !!k && typeof k.profileId === "string" && typeof k.hostId === "string" && typeof k.at === "number")
      : [];
  } catch {
    return [];
  }
}

function write(kept: Kept[]): void {
  try {
    if (kept.length) localStorage.setItem(KEY, JSON.stringify(kept));
    else localStorage.removeItem(KEY);
  } catch { /* The work itself is still kept under its old pairing. */ }
}

/** Remember whose work stays on this device once its pairing is forgotten. */
export function keepWork(profile: HostProfile): void {
  write([...read().filter((k) => k.profileId !== profile.profileId), { profileId: profile.profileId, hostId: profile.hostId, at: Date.now() }]);
}

/**
 * Move one pairing's local storage to another. The phone's command history is
 * left behind: it describes what the earlier device key heard, so sends it
 * never heard answered stay uncertain instead of being settled from it.
 * False when storage refused a value, which then stays where it was.
 */
function moveLocal(from: string, to: string): boolean {
  let moved = true;
  const copies: [string, string, string][] = [];
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (!key || key === `command-history.${from}`) continue;
    const parts = key.split("."), at = parts.indexOf(from);
    if (at < 0) continue;
    const value = localStorage.getItem(key);
    if (value === null) continue;
    parts[at] = to;
    const next = parts.join(".");
    // Only structured pairing references change; user text is never rewritten.
    let copy = value;
    if (key === `transfers.v1.${from}`) {
      const jobs = JSON.parse(value) as Record<string, { profileId?: string }>;
      for (const job of Object.values(jobs)) if (job.profileId === from) job.profileId = to;
      copy = JSON.stringify(jobs);
    }
    const existing = localStorage.getItem(next);
    if (existing !== null && existing !== copy) { moved = false; continue; }
    copies.push([key, next, copy]);
  }
  // Write every copy before removing any source. Quota or access failures
  // leave the original work intact, including values already copied.
  for (const [, next, copy] of copies) localStorage.setItem(next, copy);
  for (const [key] of copies) localStorage.removeItem(key);
  return moved;
}

/**
 * Give `next` what earlier pairings of the same computer left on this device:
 * pairings it replaced and forgotten ones. Done before the new pairing opens,
 * so nothing it loads is missing. Work that couldn't move stays noted, and the
 * next pairing of this computer tries again.
 */
export async function carryOver(next: HostProfile, replaced: string[]): Promise<void> {
  const kept = read();
  const forgotten = kept.filter((k) => k.hostId === next.hostId).sort((a, b) => b.at - a.at).map((k) => k.profileId);
  // Newest first: if two ever left the same item, the newer one is kept.
  const earlier = [...new Set([...replaced, ...forgotten])].filter((id) => id !== next.profileId);
  const left: string[] = [];
  for (const from of earlier) {
    let local = false;
    // File bytes are copied first and retained at the source for any work
    // whose local-storage copy fails. A retry never overwrites newer bytes.
    try {
      if (typeof indexedDB !== "undefined") await transferBytes.move(from, next.profileId);
      local = moveLocal(from, next.profileId);
      if (local && typeof indexedDB !== "undefined") await previewDrafts.move(from, next.profileId);
    } catch { local = false; }
    if (!local) left.push(from);
  }
  write([...kept.filter((k) => k.hostId !== next.hostId), ...left.map((profileId) => ({ profileId, hostId: next.hostId, at: Date.now() }))]);
  if (left.length)
    reportPlatformError({ operation: `Pairing with ${next.name}`, message: "Some drafts or files this device kept for this computer couldn’t move to the new pairing. The earlier copies stay on this device; a later pairing tries them again." });
}
