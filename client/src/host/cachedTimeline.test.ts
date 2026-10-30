// SPDX-License-Identifier: Apache-2.0
//
// A conversation shown from the phone's cache has no place in the journal.
// What the computer added while the app was closed must never vanish between
// the cached messages and the newest ones.
import { afterEach, expect, it, vi } from "vitest";
import { HostSession } from "./session";
import { createSimPlatform } from "../dev/sim/platform";
import { cachedState, cacheObservations } from "../platform/offline";
import type { Event, TimelineItem } from "../protocol/types";

afterEach(() => {
  window.__sim?.host.dispose();
  vi.useRealTimers();
});

it("a cached conversation reopened after a long absence has no silent hole", async () => {
  vi.useFakeTimers();
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const first = new HostSession(profile.profileId, platform.control(profile.profileId));
  first.start();
  await vi.advanceTimersByTimeAsync(900);
  const host = window.__sim!.host;
  const task = host.tasks[0]!;
  const opening = first.loadTimeline(task.task_id);
  await vi.advanceTimersByTimeAsync(100);
  await opening;
  const cachedItems = first.state.get().timelines[task.task_id]!;
  expect(cachedItems.loaded).toBe(true);
  expect(cachedItems.hasMore).toBe(false);
  // The app is closed with the conversation cached.
  cacheObservations(profile, first)();
  first.stop();

  // Meanwhile the agent keeps working: more than one page of new items.
  for (let i = 0; i < 120; i++) host.emit({ type: "timeline_appended", task_id: task.task_id, item: { seq: host.seq + 1, at: Date.now(), run_id: null, item: { kind: "note", text: `Step ${i}` } } });

  const second = new HostSession(profile.profileId, platform.control(profile.profileId));
  second.state.set(cachedState(profile)!);
  second.start();
  try {
    await vi.advanceTimersByTimeAsync(900);
    // The conversation screen asks for the newest page when it opens.
    const reopening = second.loadTimeline(task.task_id);
    await vi.advanceTimersByTimeAsync(100);
    await reopening;

    const everything = host.journal
      .filter((e): e is typeof e & { event: Extract<Event, { type: "timeline_appended" }> } => e.event.type === "timeline_appended" && e.event.task_id === task.task_id)
      .map((e) => e.event.item.seq);
    const shown = second.state.get().timelines[task.task_id]!;
    const seqs = shown.items.map((i: TimelineItem) => i.seq);
    // Whatever is shown is one unbroken stretch of the conversation...
    expect(seqs).toEqual(everything.filter((seq) => seq >= seqs[0]!));
    // ...and anything before it can still be loaded.
    expect(shown.hasMore).toBe(seqs[0]! > everything[0]!);
  } finally {
    second.stop();
  }
});
