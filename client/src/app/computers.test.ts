// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Computers, PARK_AFTER_MS } from "./computers";
import { outOfReach } from "./fleet";
import { FakeLink, platform, profile } from "../platform/lifecycle.fixtures";
import { secondProfile, thirdProfile } from "../dev/sim/lifecycle";

const spare = secondProfile(profile), laptop = thirdProfile(profile);
let fake: ReturnType<typeof platform>;
let links: Map<string, FakeLink[]>;
const opened = (id: string) => links.get(id) ?? [];
function visibility(value: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { configurable: true, value });
  document.dispatchEvent(new Event("visibilitychange"));
}

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  links = new Map();
  fake = platform();
  fake.value.control = (id) => {
    const link = new FakeLink();
    if (id !== profile.profileId) link.snap.host = { ...link.snap.host, host_id: id === spare.profileId ? spare.hostId : laptop.hostId, name: id === spare.profileId ? spare.name : laptop.name };
    links.set(id, [...opened(id), link]);
    return link;
  };
});
afterEach(() => { visibility("visible"); vi.useRealTimers(); });

it("keeps one connection to each paired computer while held, and lets go of them all after", async () => {
  const computers = new Computers(fake.value, [profile, spare, laptop]);
  expect(links.size).toBe(0);
  const release = computers.hold();
  expect([...links.keys()]).toEqual([profile.profileId, spare.profileId, laptop.profileId]);
  // Asking again, or holding twice as React's checks do, opens nothing more.
  const studio = computers.session(profile.profileId);
  expect(computers.session(profile.profileId)).toBe(studio);
  const again = computers.hold();
  release();
  await Promise.resolve();
  expect(opened(profile.profileId)).toHaveLength(1);
  expect(opened(profile.profileId)[0]!.stopped).toBe(false);
  again();
  await Promise.resolve();
  for (const id of links.keys()) expect(opened(id).every((l) => l.stopped)).toBe(true);
});

it("connects a newly paired computer, drops a forgotten one, and serves one paired since the last sync", () => {
  const computers = new Computers(fake.value, [profile]);
  computers.hold();
  // A computer just paired is shown before the paired list catches up.
  const fresh = computers.session(spare.profileId, spare);
  expect(computers.get().get(spare.profileId)).toBe(fresh);
  computers.sync([profile, spare, laptop]);
  expect(opened(laptop.profileId)).toHaveLength(1);
  expect(opened(spare.profileId)).toHaveLength(1);
  computers.sync([profile, laptop]);
  expect(opened(spare.profileId)[0]!.stopped).toBe(true);
  expect(computers.peek(spare.profileId)).toBeUndefined();
  expect(() => computers.session(spare.profileId)).toThrow("no longer paired");
  computers.forget(laptop.profileId);
  expect(opened(laptop.profileId)[0]!.stopped).toBe(true);
});

it("lets go of computers the screen doesn't show once the app has been out of sight a while, and comes back with it", () => {
  const held = new Set([laptop.profileId]);
  const computers = new Computers(fake.value, [profile, spare, laptop], { held: (id) => held.has(id) });
  computers.hold();
  computers.view(profile.profileId);
  // A quick look at another app keeps every connection.
  visibility("hidden");
  vi.advanceTimersByTime(PARK_AFTER_MS - 1000);
  visibility("visible");
  vi.advanceTimersByTime(PARK_AFTER_MS);
  expect(opened(spare.profileId)[0]!.stopped).toBe(false);
  // Longer, and only the computer on screen and one still confirming that
  // control ended keep theirs.
  visibility("hidden");
  vi.advanceTimersByTime(PARK_AFTER_MS);
  expect(opened(spare.profileId)[0]!.stopped).toBe(true);
  expect(opened(profile.profileId)[0]!.stopped).toBe(false);
  expect(opened(laptop.profileId)[0]!.stopped).toBe(false);
  expect(computers.peek(spare.profileId)).toBeUndefined();
  // Back on screen: one new connection, the only one open to it.
  visibility("visible");
  expect(opened(spare.profileId)).toHaveLength(2);
  expect(computers.peek(spare.profileId)?.link.get().state).toBe("online");
});

it("asks for the computer let go of when something needs it while the app is away", () => {
  const computers = new Computers(fake.value, [profile, spare]);
  computers.hold();
  computers.view(profile.profileId);
  visibility("hidden");
  vi.advanceTimersByTime(PARK_AFTER_MS);
  expect(computers.peek(spare.profileId)).toBeUndefined();
  // An alert for it is opened: switching to it connects it again.
  computers.view(spare.profileId);
  expect(opened(spare.profileId)).toHaveLength(2);
  expect(computers.viewing.get()).toBe(spare.profileId);
});

it("passes on a name the computer's owner gave it there, once", async () => {
  const named = vi.fn();
  fake.value.control = (id) => {
    const link = new FakeLink();
    if (id === spare.profileId) link.snap.host = { ...link.snap.host, host_id: spare.hostId, name: "Workshop", renamed: true };
    if (id === laptop.profileId) link.snap.host = { ...link.snap.host, host_id: laptop.hostId, name: "laptop-hostname" };
    links.set(id, [...opened(id), link]);
    return link;
  };
  const computers = new Computers(fake.value, [profile, spare, laptop], { named });
  computers.hold();
  await vi.advanceTimersByTimeAsync(10);
  // A name never chosen there leaves the one this device gave it.
  expect(named.mock.calls).toEqual([[spare.profileId, "Workshop"]]);
  computers.sync([profile, { ...spare, name: "Workshop" }, laptop]);
  await vi.advanceTimersByTimeAsync(10);
  expect(named).toHaveBeenCalledTimes(1);
});

it("counts every computer as shown while All is, and only the one on screen otherwise", async () => {
  const viewing: [string, boolean][] = [];
  fake.value.notify = async () => {};
  fake.value.alerts = {
    state: async () => ({ settings: { enabled: true, detail: "minimal" }, statuses: {}, android: { running: true, unrestricted: true, needsYou: true, finished: true } }),
    configure: async () => { throw new Error("unused"); }, battery: async () => {}, channel: async () => {},
    viewing: async (id, on) => { viewing.push([id, on]); },
  };
  const computers = new Computers(fake.value, [profile, spare, laptop]);
  computers.hold();
  computers.view(profile.profileId);
  expect(viewing).toEqual([[profile.profileId, true]]);
  viewing.length = 0;
  // All's list on screen, twice over (Tasks and To-Do both mounted): every
  // computer counts as shown until neither is.
  const tasks = computers.showAll(), todo = computers.showAll();
  expect(viewing.sort()).toEqual([[laptop.profileId, true], [spare.profileId, true]].sort());
  viewing.length = 0;
  tasks();
  tasks();
  expect(viewing).toEqual([]);
  todo();
  expect(viewing.sort()).toEqual([[laptop.profileId, false], [spare.profileId, false]].sort());
});

it("counts a computer trying again after it went offline as out of reach, as the header does", () => {
  expect(outOfReach({ state: "connecting", attempt: 1 })).toBe(false);
  expect(outOfReach({ state: "connecting", attempt: 2 })).toBe(true);
  expect(outOfReach({ state: "offline", since: 1, reason: "Offline", retryAt: null })).toBe(true);
  expect(outOfReach({ state: "failed", reason: "revoked", detail: "" })).toBe(true);
  expect(outOfReach({ state: "online", since: 1 })).toBe(false);
});
