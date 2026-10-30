// SPDX-License-Identifier: Apache-2.0
import { afterEach, expect, it, vi } from "vitest";
import type { HostProfile } from "../host/link";
import { HostSession } from "../host/session";
import { hostInfo, ids, projects, runs, tasks } from "../dev/sim/scenario";
import { applyTimelinePage, emptyHostState, type HostState } from "../state/projection";
import { cachedState, cacheObservations } from "./offline";

afterEach(() => { vi.useRealTimers(); localStorage.clear(); });

it("serializes each part of the observations again only when it changes, writing the same text as before", () => {
  vi.useFakeTimers();
  const byId = <T,>(list: T[], id: (t: T) => string) => Object.fromEntries(list.map((t) => [id(t), t]));
  let reads = 0;
  const counted = new Proxy(byId(tasks(1000), (t) => t.task_id), { ownKeys: (target) => { reads++; return Reflect.ownKeys(target); } });
  const start: HostState = applyTimelinePage({ ...emptyHostState, hostId: ids.host, host: hostInfo(1000), caughtUp: true, lastHeardAt: 1000,
    projects: byId(projects(1000), (p) => p.project_id), tasks: counted, runs: byId(runs(1000), (r) => r.run_id) },
    ids.tDash, [{ seq: 1, at: 1, run_id: null, item: { kind: "note", text: "Started" } }], false);
  const session = new HostSession("profile", { connect() {}, disconnect() {}, send: () => true, listen: () => () => {} });
  session.state.set(start);
  const profile = { profileId: "profile", hostId: ids.host } as HostProfile;
  const key = `read-cache.v1.${profile.profileId}`;
  const off = cacheObservations(profile, session);

  for (const heard of [2000, 3000, 4000]) {
    session.state.set((s) => ({ ...s, lastHeardAt: heard }));
    vi.advanceTimersByTime(1000);
    expect(cachedState(profile)?.lastHeardAt).toBe(heard);
  }
  expect(reads).toBe(1);
  const written = localStorage.getItem(key)!;
  expect(written).toBe(JSON.stringify(JSON.parse(written)));
  localStorage.removeItem(key);
  cacheObservations(profile, session)();
  expect(written).toBe(localStorage.getItem(key));

  let runReads = 0;
  session.state.set((s) => ({ ...s, runs: new Proxy(s.runs, { ownKeys: (target) => { runReads++; return Reflect.ownKeys(target); } }) }));
  vi.advanceTimersByTime(1000);
  runReads = 0;
  session.state.set((s) => applyTimelinePage(s, ids.tDash, [{ seq: 2, at: 2, run_id: null, item: { kind: "note", text: "Later" } }], false));
  vi.advanceTimersByTime(1000);
  expect(runReads).toBe(0);
  expect(cachedState(profile)?.timelines[ids.tDash]?.items.map((i) => i.seq)).toEqual([1, 2]);
  expect(localStorage.getItem(key)).toBe(JSON.stringify(JSON.parse(localStorage.getItem(key)!)));

  const renamed = { ...start.tasks[ids.tDash]!, title: "Renamed" };
  session.state.set((s) => ({ ...s, tasks: { ...s.tasks, [renamed.task_id]: renamed } }));
  vi.advanceTimersByTime(1000);
  expect(cachedState(profile)?.tasks[ids.tDash]?.title).toBe("Renamed");
  off();
});
