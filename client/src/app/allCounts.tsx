// SPDX-License-Identifier: Apache-2.0
//
// What All counts on every computer: what waits for the person under Needs
// you, the report questions with it, and the to-dos waiting on them. The
// tab bar and All's lists read the same counts, so a badge never counts
// what the list doesn't show. A computer out of reach counts nothing, as
// All leaves it out; the phone's own report questions count with the
// computer on screen, as All lists them.

import { useEffect, useState, type ReactNode } from "react";
import { useHost } from "./context";
import { listedEverywhere, useComputerEntries, useEveryComputer, type ComputerEntry } from "./fleet";
import { OnComputer } from "./onComputer";
import type { LinkState } from "../host/link";
import type { HostState } from "../state/projection";
import { Store, useStore } from "../state/store";
import { useReportQuestions } from "../reports/Questions";
import { signInAttention } from "./signInFeature";

export interface AllCounts { needs: number; asking: number; todo: number }
const NONE: AllCounts = { needs: 0, asking: 0, todo: 0 };

const connected = (_: ComputerEntry, s: HostState | null, link: LinkState) => link.state === "online" && !!s?.caughtUp;
const caughtUp = (s: HostState | undefined) => s?.caughtUp;
const sameFlags = (a: boolean[], b: boolean[]) => a.length === b.length && a.every((x, i) => x === b[i]);

/** Every computer's counts, summed, and the parts that read them, to render.
 * `todoBadge` is the To-Do count's own hook, handed in by those that show it
 * (importing it here split the app's styles into two files). */
export function useAllCounts(todoBadge: () => number): { counts: AllCounts; readers: ReactNode } {
  const entries = useComputerEntries();
  const online = useEveryComputer(entries, connected, sameFlags, caughtUp);
  const [into] = useState(() => new Store<Record<string, AllCounts>>({}));
  const values = useStore(into, (o) => o);
  // The computer on screen always reads, for the phone's own report questions.
  const counted = entries.map((e, i) => ({ entry: e, online: !!online[i] })).filter((c) => c.online || c.entry.current);
  const ids = counted.map((c) => c.entry.profile.profileId).join("\n");
  useEffect(() => {
    const keep = new Set(ids.split("\n"));
    into.set((o) => Object.keys(o).every((id) => keep.has(id)) ? o : Object.fromEntries(Object.entries(o).filter(([id]) => keep.has(id))));
  }, [ids, into]);
  const counts = counted.reduce((sum, c) => {
    const v = values[c.entry.profile.profileId] ?? NONE;
    return { needs: sum.needs + v.needs, asking: sum.asking + v.asking, todo: sum.todo + v.todo };
  }, NONE);
  const readers = counted.map((c) => <OnComputer key={c.entry.profile.profileId} entry={c.entry}>
    <Count id={c.entry.profile.profileId} online={c.online} phone={c.entry.current} into={into} todoBadge={todoBadge} />
  </OnComputer>);
  return { counts, readers };
}

function Count({ id, online, phone, into, todoBadge }: { id: string; online: boolean; phone: boolean; into: Store<Record<string, AllCounts>>; todoBadge: () => number }) {
  // An agent that isn't signed in is listed under Needs you too.
  const needs = useHost((s) => listedEverywhere(s).length + signInAttention(s).length);
  const asking = useReportQuestions(!phone);
  const todo = todoBadge();
  const value = online ? { needs, asking, todo } : { needs: 0, asking, todo: 0 };
  useEffect(() => {
    into.set((o) => { const v = o[id]; return v && v.needs === value.needs && v.asking === value.asking && v.todo === value.todo ? o : { ...o, [id]: value }; });
  });
  useEffect(() => () => into.set((o) => { if (!(id in o)) return o; const { [id]: _gone, ...rest } = o; return rest; }), [id, into]);
  return null;
}
