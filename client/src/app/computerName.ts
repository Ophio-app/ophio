// SPDX-License-Identifier: Apache-2.0
//
// Where a computer's name is kept. A computer that keeps its own name shows
// the one its owner gives it on every phone and tablet paired with it; one
// from before that keeps the name each device gives it.

import type { HostProfile, LinkState } from "../host/link";
import type { HostState } from "../state/projection";
import { PRODUCT_NAME } from "../brand";

/** The computer keeps a name its owner gives it and shares it. Older
 * computers drop the connection on a request they don't know, so the phone
 * asks only one that says it can. */
export const COMPUTER_NAME_FEATURE = "computer_name_v1";

/** How renaming a computer from this device goes:
 * - `computer`: the computer takes the name, for every device;
 * - `wait`: the computer keeps its name but can't take one from here now;
 * - `phone`: only this device uses the name, as before. */
export type RenameWay = "computer" | "wait" | "phone";

export function keepsName(s: Pick<HostState, "capabilities" | "host">): boolean {
  return !!(s.capabilities ?? s.host?.capabilities)?.features.includes(COMPUTER_NAME_FEATURE);
}

export function renameWay(s: Pick<HostState, "capabilities" | "host" | "grants">, link: LinkState): RenameWay {
  if (!keepsName(s)) return "phone";
  if (s.grants?.admin === true && link.state === "online") return "computer";
  // A device that may not manage the computer can still name it for itself,
  // unless its owner named it there: that name comes back on every connect.
  if (s.grants?.admin === false && !s.host?.renamed) return "phone";
  return "wait";
}

/** What the rename sheet says about who sees the name. */
export function renameHint(way: RenameWay, computer: string, device: string, s: Pick<HostState, "capabilities" | "host" | "grants">, link: LinkState): string {
  switch (way) {
    case "computer":
      return `The new name is kept on ${computer}, so every phone and tablet paired with it shows it.`;
    case "wait":
      return s.grants?.admin === false
        ? `${computer}’s owner named it there. Only a device allowed to manage ${computer} can rename it.`
        : link.state === "online"
          ? `${computer} is still connecting. Try again in a moment.`
          : `Connect to ${computer} to rename it. It keeps its name, so every phone and tablet paired with it shows the same one.`;
    case "phone":
      return keepsName(s)
        ? `Only this ${device} uses this name. This ${device} isn’t allowed to rename ${computer} itself.`
        : `Only this ${device} uses this name. Update ${PRODUCT_NAME} on ${computer} so every phone and tablet paired with it shows the same name.`;
  }
}

/** The longest name a computer takes. */
export const COMPUTER_NAME_MAX = 60;

const key = (name: string) => name.trim().toLocaleLowerCase();

/** Profiles whose name another paired computer also has: a reinstalled
 * computer, or an old pairing at another port. */
export function sharedNames(profiles: readonly HostProfile[]): Set<string> {
  const seen = new Map<string, number>();
  for (const p of profiles) seen.set(key(p.name), (seen.get(key(p.name)) ?? 0) + 1);
  return new Set(profiles.filter((p) => (seen.get(key(p.name)) ?? 0) > 1).map((p) => p.profileId));
}

const ADOPTED = "computer-names-adopted.v1";
function adopted(): Record<string, string> {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(ADOPTED) ?? "{}");
    return value && typeof value === "object" && !Array.isArray(value)
      ? Object.fromEntries(Object.entries(value).filter((e): e is [string, string] => typeof e[1] === "string")) : {};
  } catch { return {}; }
}
function keep(names: Record<string, string>): void {
  try { localStorage.setItem(ADOPTED, JSON.stringify(names)); } catch { /* Then a name from the computer lasts only this run. */ }
}

/** The name this device took from the computer itself, if its name for it
 * is still that one. Such a name follows the computer's, even back to its
 * hostname, which the computer announces as never chosen; a name the
 * person gave on this device alone stays. */
export const takenName = (profileId: string): string | undefined => adopted()[profileId];
export function takeName(profileId: string, name: string): void { keep({ ...adopted(), [profileId]: name }); }
export function dropTakenName(profileId: string): void {
  const names = adopted();
  if (!(profileId in names)) return;
  delete names[profileId];
  keep(names);
}
