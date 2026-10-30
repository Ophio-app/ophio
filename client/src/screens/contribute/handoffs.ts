// SPDX-License-Identifier: Apache-2.0
//
// Handoffs from Clankerhub kept on this phone until they're opened or expire
// (CH-016 D-7). A handoff is two JSON records the phone carries unchanged to
// the computer the person chooses, whose receiver checks them; the phone
// trusts neither. Before that check it reads only two things to show: when
// the handoff expires, and the game's id when it is a plain id.
//
// Kept for the phone, not one computer: the person may choose any paired
// computer for it, and switching computers keeps it waiting.

import type { ContributionView } from "../../protocol/types";
import { Store } from "../../state/store";

const KEY = "contribution-handoffs.v1";
const FOLDERS = "contribution-folders.v1";
/** The receiver's own limit on each record; anything bigger is never kept. */
export const RECORD_LIMIT = 65_536;
/** A handoff whose expiry can't be read waits a day, the longest the
 * contract allows, then goes. */
const UNREADABLE_LIFETIME_MS = 86_400_000;

export interface Handoff {
  /** Phone-local identity. */
  id: string;
  /** Exact bytes as received; never reserialized. */
  packetJson: string;
  handoffJson: string;
  receivedAt: number;
  /** The packet's own expiry, or null when it doesn't say so readably. */
  expiresAt: number | null;
  /** The game's id when it is a plain id; not checked until a computer does. */
  gameId: string | null;
  /** The computer whose receiver checked it, and that receiver's operation. */
  hostId: string | null;
  operationId: string | null;
  /** The receiver's last word, kept for the waiting list and a restart. */
  view: ContributionView | null;
  /** The receiver was asked to download: from then on "nothing was
   * downloaded" may not be true. */
  started: boolean;
  /** Counts the person's changes and the phone's requests. A reply to a
   * request made before the latest is out of date and is ignored. */
  generation: number;
  /** The latest request to download, whose final result is followed. */
  prepareCommand: string | null;
  /** A request to download whose answer hasn't arrived: kept before it's
   * sent, so a dropped connection or a restart asks the computer about it
   * instead of assuming nothing happened. */
  unsettled: string | null;
}

/** Plain ids, as the receiver accepts them. Anything else isn't shown. */
const ID = /^[a-z0-9][a-z0-9._-]{0,79}$/;

function peek(packetJson: string, handoffJson: string): Pick<Handoff, "expiresAt" | "gameId"> {
  let expiresAt: number | null = null;
  let gameId: string | null = null;
  try {
    const p: unknown = JSON.parse(packetJson);
    const at = p && typeof p === "object" ? (p as { expiresAt?: unknown }).expiresAt : undefined;
    if (typeof at === "number" && Number.isSafeInteger(at) && at > 0) expiresAt = at * 1000;
  } catch { /* The receiver says what's wrong with it. */ }
  try {
    const h: unknown = JSON.parse(handoffJson);
    const id = h && typeof h === "object" ? (h as { gameId?: unknown }).gameId : undefined;
    if (typeof id === "string" && ID.test(id)) gameId = id;
  } catch { /* As above. */ }
  return { expiresAt, gameId };
}

function valid(h: unknown): h is Handoff {
  if (!h || typeof h !== "object") return false;
  const x = h as Partial<Handoff>;
  return typeof x.id === "string" && typeof x.packetJson === "string" && typeof x.handoffJson === "string"
    && typeof x.receivedAt === "number" && (x.expiresAt === null || typeof x.expiresAt === "number")
    && (x.gameId === null || typeof x.gameId === "string") && (x.hostId === null || typeof x.hostId === "string")
    && (x.operationId === null || typeof x.operationId === "string") && typeof x.started === "boolean";
}

function load(): Handoff[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(value)
      ? value.filter(valid).map((h) => ({ ...h, generation: h.generation ?? 0, prepareCommand: h.prepareCommand ?? null, unsettled: h.unsettled ?? null }))
      : [];
  } catch {
    return [];
  }
}

/** The handoff's expiry, or the longest a handoff can last when it doesn't say. */
export const expiry = (h: Handoff): number => h.expiresAt ?? h.receivedAt + UNREADABLE_LIFETIME_MS;

/** Downloading or checking on the computer, or stopping: it keeps its slot
 * there whatever the packet's expiry says. */
export const busy = (h: Handoff): boolean =>
  !!h.view && (h.view.code === "acquiring" || h.view.code === "verifying" || h.view.code === "reconcile_required" || h.view.interruption_pending);

export class Handoffs extends Store<Handoff[]> {
  /** Recovery details changed in memory but could not be saved on the phone. */
  readonly persistenceFailed = new Store(false);
  /** How many expired handoffs that never downloaded anything were removed,
   * so the waiting list can say so once. */
  expiredRemoved = 0;

  constructor() {
    super(load());
  }

  #save(next: Handoff[], durable = false): boolean {
    try {
      if (next.length) localStorage.setItem(KEY, JSON.stringify(next));
      else localStorage.removeItem(KEY);
    } catch {
      this.persistenceFailed.set(true);
      // Ordinary updates still show the computer's latest answer. A download
      // request or removal, however, must not take effect unless saved.
      if (!durable) this.set(next);
      return false;
    }
    this.persistenceFailed.set(false);
    this.set(next);
    return true;
  }

  /** Retry saving recovery details only; never sends a download request. */
  retrySave(): boolean {
    return this.#save(this.get());
  }

  find(id: string): Handoff | undefined {
    return this.get().find((h) => h.id === id);
  }

  /**
   * Keep a handoff for the person to open. The same records again are the
   * same handoff, so a second press reaches the one already here. Null when
   * either record is larger than any handoff may be: the phone keeps nothing
   * it would only have to refuse.
   */
  receive(packetJson: string, handoffJson: string, now = Date.now()): Handoff | null {
    if (packetJson.length > RECORD_LIMIT || handoffJson.length > RECORD_LIMIT
      || new TextEncoder().encode(packetJson).byteLength > RECORD_LIMIT
      || new TextEncoder().encode(handoffJson).byteLength > RECORD_LIMIT) return null;
    const same = this.get().find((h) => h.packetJson === packetJson && h.handoffJson === handoffJson);
    if (same) return same;
    const h: Handoff = {
      id: crypto.randomUUID(),
      packetJson,
      handoffJson,
      receivedAt: now,
      ...peek(packetJson, handoffJson),
      hostId: null,
      operationId: null,
      view: null,
      started: false,
      generation: 0,
      prepareCommand: null,
      unsettled: null,
    };
    this.#save([...this.get(), h]);
    return h;
  }

  update(id: string, patch: Partial<Omit<Handoff, "id" | "packetJson" | "handoffJson" | "receivedAt">>, { durable = false }: { durable?: boolean } = {}): boolean {
    const all = this.get();
    if (!all.some((h) => h.id === id)) return false;
    return this.#save(all.map((h) => (h.id === id ? { ...h, ...patch } : h)), durable);
  }

  /** Done with it: opened, dismissed or finished. The computer keeps
   * whatever it added; this only forgets the phone's copy. */
  remove(id: string): void {
    const all = this.get();
    if (all.some((h) => h.id === id)) this.#save(all.filter((h) => h.id !== id), true);
  }

  /**
   * Remove handoffs past their expiry that never started downloading. One
   * that started, or may have, stays until the person has seen what happened
   * to it, and one the computer is still working on stays until it settles.
   */
  prune(now = Date.now()): void {
    // Keep unsaved handoffs visible with their recovery warning.
    if (this.persistenceFailed.get()) return;
    const all = this.get();
    const keep = all.filter((h) => busy(h) || h.started || h.unsettled !== null || expiry(h) > now);
    if (keep.length === all.length) return;
    if (this.#save(keep, true)) this.expiredRemoved += all.length - keep.length;
  }
}

let shared: Handoffs | null = null;
/** The phone's handoffs. */
export function handoffs(): Handoffs {
  return (shared ??= new Handoffs());
}
/** Tests start from what localStorage holds now. */
export function resetHandoffs(): void {
  shared = null;
}

/** The Library folder last chosen for contribution projects on each computer. */
export function chosenFolder(hostId: string): string | null {
  try {
    const all: unknown = JSON.parse(localStorage.getItem(FOLDERS) ?? "{}");
    const id = all && typeof all === "object" ? (all as Record<string, unknown>)[hostId] : undefined;
    return typeof id === "string" ? id : null;
  } catch {
    return null;
  }
}

export function chooseFolder(hostId: string, rootId: string): void {
  try {
    const all: unknown = JSON.parse(localStorage.getItem(FOLDERS) ?? "{}");
    const next = all && typeof all === "object" && !Array.isArray(all) ? { ...(all as Record<string, string>) } : {};
    next[hostId] = rootId;
    localStorage.setItem(FOLDERS, JSON.stringify(next));
  } catch { /* Asked again next time. */ }
}
