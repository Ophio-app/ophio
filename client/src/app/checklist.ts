// SPDX-License-Identifier: Apache-2.0
//
// A project's checklist: what the person has to do themselves, as agents put
// it there. The rows under New in Chat and in Tasks read the summaries; the
// checklist's own screen reads one project's whole list. Both ask again
// whenever the computer says a checklist changed.

import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import { commandResult } from "../host/commandResult";
import type { HostSession } from "../host/session";
import type { Checklist, ChecklistItem, ChecklistSummary, Command, FileDeliveryView } from "../protocol/types";
import { PRODUCT_NAME } from "../brand";
import { describeError, useApp, useHost, useLink, shallowEqual } from "./context";

/** The computer's word that it keeps checklists. */
export const CHECKLIST_FEATURE = "checklist_v1";
export const CHECKLIST_UPDATE = `Update ${PRODUCT_NAME} on this computer to keep a checklist of what you have to do yourself.`;

/** What Request checklist asks for, in the new task's message. */
export const REQUEST_TEXT = "Make a checklist of everything I have to do myself in this project, such as accounts, access, keys, payments, choices and checks, with complete steps for each.";

/** The setup skill that tells an agent how to write one. */
export const CHECKLIST_SKILL = "checklist";

/** A tick waits for the computer's word; the agent waiting on it starts after. */
const ACT_WAIT_MS = 20_000;
/** Taildrop, a drive or a folder can take a while for a large file. */
const DELIVER_WAIT_MS = 15 * 60_000;

export function useChecklistSupported(): boolean {
  return useHost((s) => !!s.capabilities?.features.includes(CHECKLIST_FEATURE));
}

type Summaries = { revision: number; read: Promise<ChecklistSummary[]> };
const summaries = new WeakMap<HostSession, Summaries>();
const known = new WeakMap<HostSession, ChecklistSummary[]>();

/** Every project's checklist at a glance. Rows in two tabs share one question
 * per change. */
function readSummaries(session: HostSession, revision: number): Promise<ChecklistSummary[]> {
  const asked = summaries.get(session);
  if (asked && asked.revision === revision) return asked.read;
  const read = session.query({ kind: "checklists" }).then((r) => {
    known.set(session, r.checklists);
    return r.checklists;
  });
  // A failed question is asked again next time.
  read.catch(() => { if (summaries.get(session)?.read === read) summaries.delete(session); });
  summaries.set(session, { revision, read });
  return read;
}

/**
 * `projectId`'s checklist at a glance: null while it has none, undefined
 * until the computer has said.
 */
export function useChecklistSummary(projectId: string): ChecklistSummary | null | undefined {
  const { session } = useApp();
  const online = useLink().state === "online";
  const supported = useChecklistSupported();
  const allowed = useHost((s) => s.grants?.tasks !== false);
  const revision = useHost((s) => s.checklistsRevision);
  const find = (all: ChecklistSummary[] | undefined) => all && (all.find((c) => c.project_id === projectId) ?? null);
  const [summary, setSummary] = useState(() => find(known.get(session)));
  useEffect(() => {
    setSummary(find(known.get(session)));
    if (!online || !supported || !allowed) return;
    let alive = true;
    readSummaries(session, revision).then((all) => { if (alive) setSummary(find(all)); }, () => {});
    return () => { alive = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session, projectId, online, supported, allowed, revision]);
  return summary;
}

/** A tick, an answer or a question this phone sent, shown until the
 * computer's own list has it. */
export type Pending =
  | { kind: "tick"; itemId: string; stepId?: string; done: boolean }
  | { kind: "answer"; itemId: string; answer: string };

type CachedList = {
  list?: Checklist | null;
  error: string | null;
  pending: Map<string, Pending>;
  accepted: Set<Pending>;
  read?: Promise<void>;
  revision?: string;
  serial: number;
};
type ChecklistCache = { entries: Map<string, CachedList>; listeners: Set<() => void>; version: number };
const caches = new WeakMap<HostSession, ChecklistCache>();
function cacheFor(session: HostSession): ChecklistCache {
  let cache = caches.get(session);
  if (!cache) caches.set(session, cache = { entries: new Map(), listeners: new Set(), version: 0 });
  return cache;
}
function entryFor(cache: ChecklistCache, id: string): CachedList {
  let entry = cache.entries.get(id);
  if (!entry) cache.entries.set(id, entry = { error: null, pending: new Map(), accepted: new Set(), serial: 0 });
  return entry;
}
function changed(cache: ChecklistCache) {
  cache.version++;
  cache.listeners.forEach((listener) => listener());
}
function readList(session: HostSession, id: string, revision: string, computer: string, force = false): Promise<void> {
  const cache = cacheFor(session), entry = entryFor(cache, id);
  if (!force && entry.revision === revision) return entry.read ?? Promise.resolve();
  entry.revision = revision;
  const serial = ++entry.serial;
  // Only a read begun after acceptance may settle that change. Older replies
  // cannot take off a newer tick, even when several commands finish together.
  const settling = new Set(entry.accepted);
  const read = session.query({ kind: "checklist", project_id: id }).then(({ checklist }) => {
    if (serial !== entry.serial) return;
    entry.list = checklist;
    entry.error = null;
    for (const [key, change] of entry.pending) if (settling.has(change)) entry.pending.delete(key);
    for (const change of settling) entry.accepted.delete(change);
    changed(cache);
  }, (error: unknown) => {
    if (serial !== entry.serial) return;
    entry.error = describeError(error, computer);
    entry.revision = undefined;
    changed(cache);
    throw error;
  });
  entry.read = read;
  return read;
}

/** Shared per-project reads and optimistic ticks. All, project views and the
 * tab badge see the very same pending state, including before a reply. */
export function useChecklists(projectIds: readonly string[]) {
  const { session, profile } = useApp();
  const online = useLink().state === "online";
  const supported = useChecklistSupported();
  const allowed = useHost((s) => s.grants?.tasks !== false);
  const epoch = useHost((s) => s.checklistEpoch);
  const projectRevisions = useHost((s) => s.checklistProjects);
  const revisionFor = (id: string) => `${epoch}/${projectRevisions[id] ?? 0}`;
  const cache = cacheFor(session);
  const subscribe = useCallback((listener: () => void) => {
    cache.listeners.add(listener);
    return () => { cache.listeners.delete(listener); };
  }, [cache]);
  useSyncExternalStore(subscribe, () => cache.version);
  const key = JSON.stringify(projectIds);
  useEffect(() => {
    if (!online || !supported || !allowed) return;
    for (const id of JSON.parse(key) as string[]) void readList(session, id, revisionFor(id), profile.name).catch(() => {});
  }, [session, key, online, supported, allowed, epoch, projectRevisions, profile.name]);
  // Reconnecting asks again even if no revision event was delivered offline.
  useEffect(() => {
    if (!online) for (const entry of cache.entries.values()) entry.revision = undefined;
  }, [cache, online]);

  const sendFor = (projectId: string) => async (command: Command, label: string, change?: Pending, waitMs = ACT_WAIT_MS) => {
    const entry = entryFor(cache, projectId);
    const key = change && pendingKey(change);
    if (change) { entry.pending.set(key!, change); changed(cache); }
    let result;
    try {
      const receipt = await session.command(command, { label });
      result = await commandResult(session, receipt, { waitMs });
    } catch (error) {
      if (change && entry.pending.get(key!) === change) entry.pending.delete(key!);
      changed(cache);
      throw new Error(describeError(error, profile.name));
    }
    if (change) entry.accepted.add(change);
    // Acceptance and refreshing are different outcomes. A failed read leaves
    // the accepted tick in place and offers Retry, never a false refusal or
    // a second send of feedback that the computer has already taken.
    const current = session.state.get();
    await readList(session, projectId, `${current.checklistEpoch}/${current.checklistProjects[projectId] ?? 0}`, profile.name, true).catch(() => {});
    return result;
  };
  return new Map(projectIds.map((id) => {
    const entry = entryFor(cache, id);
    return [id, {
      list: entry.list ? withPending(entry.list, entry.pending) : entry.list,
      error: entry.error,
      reload: () => { void readList(session, id, revisionFor(id), profile.name, true).catch(() => {}); },
      send: sendFor(id), supported, online,
    }] as const;
  }));
}

/** One project's view over the same cache All uses. */
export function useChecklist(projectId: string) {
  return useChecklists([projectId]).get(projectId)!;
}

/** Open things the person owns that are holding an agent up. */
export function waitingOnPerson(list: Checklist): ChecklistItem[] {
  const items = allItems(list);
  const prerequisites = new Set(items.filter((i) => i.owner === "agent" && i.done_at === undefined).flatMap((i) => i.after ?? []));
  return items.filter((i) => i.owner !== "agent" && i.done_at === undefined && (!!i.waiting || prerequisites.has(i.item_id)));
}

export function useTodoBadge(): number {
  const allowed = useHost((s) => s.grants?.tasks !== false && !!s.capabilities?.features.includes(CHECKLIST_FEATURE));
  const ids = useHost((s) => Object.keys(s.projects).sort(), shallowEqual);
  const lists = useChecklists(ids);
  if (!allowed) return 0;
  return [...lists.values()].reduce((n, value) => n + (value.list ? waitingOnPerson(value.list).length : 0), 0);
}

const pendingKey = (p: Pending) => p.kind === "tick" ? `t/${p.itemId}/${p.stepId ?? ""}` : `a/${p.itemId}`;

/** `list` as it will be once what this phone sent is done. */
export function withPending(list: Checklist, pending: ReadonlyMap<string, Pending>): Checklist {
  if (!pending.size) return list;
  const now = Date.now();
  const sections = list.sections.map((section) => ({
    ...section,
    items: section.items.map((item) => {
      let next = item;
      for (const p of pending.values()) {
        if (p.itemId !== item.item_id) continue;
        next = p.kind === "answer" ? answered(next, p.answer, now) : ticked(next, p.stepId, p.done, now);
      }
      return next;
    }),
  }));
  return { ...list, sections };
}

function answered(item: ChecklistItem, text: string, at: number): ChecklistItem {
  if (!item.ask || item.owner === "agent") return item;
  return { ...item, ask: { ...item.ask, answer: { text, at } }, done_at: item.steps.every((s) => s.done_at !== undefined) ? item.done_at ?? at : item.done_at };
}

/** As the computer ticks: the last step finishes the item once its question
 * is answered, and a step unticked opens it again. */
export function ticked(item: ChecklistItem, stepId: string | undefined, done: boolean, at: number): ChecklistItem {
  if (item.owner === "agent") return item;
  if (stepId === undefined) {
    return {
      ...item,
      done_at: done ? item.done_at ?? at : undefined,
      steps: item.steps.map((s) => ({ ...s, done_at: done ? s.done_at ?? at : s.done_at })),
    };
  }
  const steps = item.steps.map((s) => s.step_id === stepId ? { ...s, done_at: done ? s.done_at ?? at : undefined } : s);
  const finished = steps.every((s) => s.done_at !== undefined) && (!item.ask || !!item.ask.answer);
  return { ...item, steps, done_at: done ? (finished ? item.done_at ?? at : item.done_at) : undefined };
}

/** Every item, in the order the checklist gives them. */
export function allItems(list: Checklist): ChecklistItem[] {
  return list.sections.flatMap((s) => s.items);
}

/** The person's day, as checklist days are written: YYYY-MM-DD. */
export function today(at = new Date()): string {
  const two = (n: number) => String(n).padStart(2, "0");
  return `${at.getFullYear()}-${two(at.getMonth() + 1)}-${two(at.getDate())}`;
}

/** Whether the day suggested for `item` has come: it's still open, or it
 * was done on that day or after, so ticking it off doesn't move it. */
export function dueNow(item: ChecklistItem, day: string): boolean {
  if (!item.due || item.due > day) return false;
  return item.done_at === undefined || today(new Date(item.done_at)) >= item.due;
}

export interface ChecklistUrgency { tier: number; priority: ChecklistItem["priority"]; due?: string }
const priorities = { blocker: 0, high: 1, medium: 2, low: 3 };
export function compareChecklistUrgency(a: ChecklistUrgency, b: ChecklistUrgency): number {
  const due = (a.due ?? "9999").localeCompare(b.due ?? "9999");
  return a.tier - b.tier || (a.tier === 1 ? due : 0) || priorities[a.priority] - priorities[b.priority] || due;
}

/** A prerequisite is at least as pressing as the thing that needs it. Carry
 * the whole ordering key, not just its tier, so it stays ahead on that day. */
export function checklistUrgency(list: Checklist, day = today()): Map<string, ChecklistUrgency> {
  const items = allItems(list);
  // Ranking stays sticky after completion, though it no longer places a done
  // item: those go last, in Done. The badge and task entries still use
  // open-only waitingOnPerson.
  const waiting = new Set(items.filter((item) => item.owner !== "agent" && (item.waiting || items.some((dependent) =>
    dependent.owner === "agent" && dependent.after?.includes(item.item_id)
    && (dependent.done_at === undefined || (item.done_at !== undefined && item.done_at <= dependent.done_at))
  ))).map((item) => item.item_id));
  const ranks = new Map(items.map((item) => [item.item_id, {
    tier: item.priority === "blocker" || waiting.has(item.item_id) ? 0 : dueNow(item, day) ? 1 : 2,
    priority: item.priority, due: item.due,
  }]));
  for (let round = 0, changed = true; changed && round < items.length; round++) {
    changed = false;
    for (const item of items) for (const id of item.after ?? []) {
      const first = ranks.get(id), mine = ranks.get(item.item_id)!;
      if (first && compareChecklistUrgency(mine, first) < 0) { ranks.set(id, { ...mine }); changed = true; }
    }
  }
  return ranks;
}

/** Global order, with open prerequisites before their dependents on a tie. */
export function orderedChecklistItems(list: Checklist, day = today()): ChecklistItem[] {
  const items = allItems(list), ranks = checklistUrgency(list, day);
  const byId = new Map(items.map((item) => [item.item_id, item]));
  const seen = new Set<string>(), result: ChecklistItem[] = [];
  const add = (item: ChecklistItem) => {
    if (seen.has(item.item_id)) return;
    seen.add(item.item_id);
    for (const id of item.after ?? []) {
      const first = byId.get(id);
      if (first && first.done_at === undefined) add(first);
    }
    result.push(item);
  };
  [...items].sort((a, b) => compareChecklistUrgency(ranks.get(a.item_id)!, ranks.get(b.item_id)!)).forEach(add);
  return result;
}

/** Done items stay in sight this long. The computer keeps them after, so
 * agents still see them done and what comes after them still holds. */
export const DONE_SHOWN_MS = 3 * 24 * 60 * 60_000;
/** The section the done items go in, last. */
export const DONE_SECTION = "--done";

/** Whether `item` still shows: open, or done in the last three days. */
export function stillShown(item: ChecklistItem, now = Date.now()): boolean {
  return item.done_at === undefined || now - item.done_at <= DONE_SHOWN_MS;
}

/** Keep section titles while allowing a pressing item to rise past a less
 * pressing section. A split section repeats its title, never its items.
 * Done items leave their sections for one Done group at the end, the latest
 * first, until they're three days old; `keep` shows ones opened by link. */
export function arranged(list: Checklist, day = today(), { now = Date.now(), keep }: { now?: number; keep?: ReadonlySet<string> } = {}): Checklist {
  const source = new Map(list.sections.flatMap((section) => section.items.map((item) => [item.item_id, section] as const)));
  const sections: Checklist["sections"] = [];
  const counts = new Map<string, number>();
  let previous: string | undefined;
  for (const item of orderedChecklistItems(list, day)) {
    if (item.done_at !== undefined) continue;
    const section = source.get(item.item_id)!;
    if (previous === section.section_id) sections.at(-1)!.items.push(item);
    else {
      const n = (counts.get(section.section_id) ?? 0) + 1;
      counts.set(section.section_id, n);
      sections.push({ ...section, section_id: n === 1 ? section.section_id : `${section.section_id}--${n}`, items: [item] });
      previous = section.section_id;
    }
  }
  const done = allItems(list).filter((item) => item.done_at !== undefined && (stillShown(item, now) || !!keep?.has(item.item_id)))
    .sort((a, b) => b.done_at! - a.done_at!);
  if (done.length) sections.push({ section_id: DONE_SECTION, title: "Done", items: done });
  return { ...list, sections };
}

/** Whether `item`'s `after` items are all done. */
export function ready(item: ChecklistItem, list: Checklist): boolean {
  if (!item.after?.length) return true;
  const done = new Set(allItems(list).filter((i) => i.done_at !== undefined).map((i) => i.item_id));
  const here = new Set(allItems(list).map((i) => i.item_id));
  return item.after.every((id) => done.has(id) || !here.has(id));
}

/** The next ready person item; never ask the person to do blocked agent work. */
export function nextItem(list: Checklist): ChecklistItem | null {
  const open = allItems(arranged(list)).filter((i) => i.owner !== "agent" && i.done_at === undefined);
  return open.find((i) => ready(i, list)) ?? null;
}

/** Questions the person asked that no agent has answered yet, for `notes`. */
export function unanswered(notes: ChecklistItem["notes"]): number {
  let open = 0;
  for (const n of notes ?? []) open = n.from === "person" ? open + 1 : 0;
  return open;
}

/** Where the computer is set to put checklist files, and where it could. */
export function useFileDelivery(enabled = true) {
  const { session, profile } = useApp();
  const online = useLink().state === "online";
  const supported = useChecklistSupported();
  const [view, setView] = useState<FileDeliveryView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [asked, setAsked] = useState(0);
  const reload = useCallback(() => setAsked((n) => n + 1), []);
  useEffect(() => {
    if (!enabled || !online || !supported) return;
    let alive = true;
    session.query({ kind: "file_delivery" }).then(
      (r) => { if (alive) { setView(r.view); setError(null); } },
      (e: unknown) => { if (alive) setError(describeError(e, profile.name)); },
    );
    return () => { alive = false; };
  }, [session, profile.name, online, supported, enabled, asked]);
  return { view, error, reload, supported, online };
}

/** In words, where checklist files go. */
export function deliveryWords(view: FileDeliveryView | null): string {
  const d = view?.settings.delivery;
  if (!d) return "To this phone";
  switch (d.kind) {
    case "phone": return "To this phone";
    case "taildrop": return `To ${d.device}, by Taildrop`;
    case "folder": return `Into ${view.folders.find((f) => f.path === d.path)?.label ?? d.path}`;
    case "drive": return `Onto ${d.remote}${d.folder ? `, in ${d.folder}` : ""}`;
  }
}

/** A `#rrggbb` colour's relative luminance, or null for anything else. */
export function luminance(hex: string | undefined): number | null {
  const m = hex && /^#([0-9a-f]{6})$/i.exec(hex);
  if (!m) return null;
  const [r, g, b] = [0, 2, 4].map((i) => {
    const c = parseInt(m[1]!.slice(i, i + 2), 16) / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!;
}

export { ACT_WAIT_MS as CHECKLIST_WAIT_MS, DELIVER_WAIT_MS };
