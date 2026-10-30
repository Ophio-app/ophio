// SPDX-License-Identifier: Apache-2.0
//
// How current the phone's picture of the computer is, in words. Derived only
// from the link state and when the host last said something.

import type { LinkState } from "../host/link";
import { shortAge } from "../design/time";

export interface Freshness {
  label: string;
  tone: "ok" | "neutral" | "warn" | "stop";
  detail: string;
  /** True when what is on screen is current. */
  live: boolean;
}

/**
 * The native link is the one judge of a dead connection. It goes offline after
 * 45 s without hearing anything, though the app pings every 15 s, and it
 * counts bytes, so a slow link still delivering a large message is not taken
 * for silence. Until then an open link is current, unless a request has gone
 * `unanswered` for a few seconds or the device reports no network: the link
 * may be dead but hasn't given up, so it is Checking. After it gives up, the
 * words are its reason and when the computer was last heard.
 */
export function freshness(link: LinkState, caughtUp: boolean, lastHeardAt: number | null, now: number, computer: string, unanswered = false): Freshness {
  const heard = lastHeardAt ? `Last heard ${ago(now - lastHeardAt)}` : "Nothing received yet";
  switch (link.state) {
    case "online":
      if (!caughtUp) return { label: "Catching up", tone: "neutral", live: false, detail: sentences(`Loading what changed on ${computer}`) };
      if (unanswered) return { label: "Checking…", tone: "neutral", live: false, detail: sentences(`Waiting for ${computer} to answer`, heard) };
      return { label: "Live", tone: "ok", live: true, detail: sentences(`Connected to ${computer}`) };
    case "connecting":
      return { label: "Connecting", tone: "neutral", live: false, detail: sentences(link.attempt > 1 ? `Attempt ${link.attempt}` : "", heard) };
    case "authenticating":
      return { label: "Connecting", tone: "neutral", live: false, detail: sentences(`Checking ${computer}’s identity`, heard) };
    case "offline": {
      const wait = link.retryAt ? link.retryAt - now : null;
      const retry = wait === null ? "" : wait < 5_000 ? "Trying again now" : `Trying again in ${shortAge(wait)}`;
      return { label: "Offline", tone: "warn", live: false, detail: sentences(link.reason, heard, retry) };
    }
    case "failed":
      return { label: FAILED[link.reason], tone: "stop", live: false, detail: sentences(link.detail) };
    case "idle":
      return { label: "Not connected", tone: "neutral", live: false, detail: sentences(heard) };
  }
}

/** One sentence per part, each ended exactly once: the link's reasons come
 * from the phone, the computer and the platform, and some already end. */
function sentences(...parts: string[]): string {
  return parts.map((p) => p.trim()).filter(Boolean).map((p) => /[.!?…]["'”’)\]]*$/.test(p) ? p : `${p}.`).join(" ");
}

/** `shortAge` says "now" under five seconds, which cannot take "ago". */
function ago(ms: number): string {
  return ms < 5_000 ? "just now" : `${shortAge(ms)} ago`;
}

const FAILED: Record<Extract<LinkState, { state: "failed" }>["reason"], string> = {
  revoked: "Removed",
  incompatible_version: "Update needed",
  unauthenticated: "Not recognized",
  not_paired: "Not paired",
  unreachable: "Unreachable",
};
