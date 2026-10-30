// SPDX-License-Identifier: Apache-2.0
//
// The computer answers queries ahead of journal events it has already queued
// for this phone. A snapshot a screen asks for must not move the phone's place
// in the journal past events still on their way: they would be dropped as
// replays, and with them what a snapshot does not carry, such as the
// conversation and command progress.
import { act, cleanup, renderHook } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, beforeEach, expect, it } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { allGrants } from "../dev/sim/host";
import * as fx from "../dev/sim/scenario";
import { platform, profile, snapshot } from "../platform/lifecycle.fixtures";
import type { ClientFrame, EventEnvelope, QueryData, Snapshot } from "../protocol/types";
import { refreshFromSnapshot } from "../state/projection";
import { useControl } from "../screens/control/useControl";
import type { Picture } from "../screens/control/useMedia";
import type { ControlLink, LinkEvent } from "./link";
import { HostSession } from "./session";

class HeldLink implements ControlLink {
  sent: ClientFrame[] = [];
  #listener: ((e: LinkEvent) => void) | undefined;
  connect() {}
  disconnect() {}
  send(frame: ClientFrame) {
    this.sent.push(frame);
    return true;
  }
  listen(listener: (e: LinkEvent) => void) {
    this.#listener = listener;
    return () => { this.#listener = undefined; };
  }
  emit(e: LinkEvent) {
    this.#listener?.(e);
  }
  /** Answer the latest unanswered query of this kind. */
  answer(data: QueryData) {
    const answered = new Set(this.#answered);
    const query = this.sent.findLast((f) => f.type === "query" && f.query.kind === data.kind && !answered.has(f.query_id));
    if (query?.type !== "query") throw new Error(`No ${data.kind} query`);
    this.#answered.push(query.query_id);
    this.emit({ type: "frame", frame: { type: "query_result", query_id: query.query_id, result: { Ok: data } } });
  }
  #answered: string[] = [];
}

const flush = () => act(async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); });
const note = (seq: number, taskId: string): EventEnvelope => ({
  seq, at: seq, source: { kind: "companion" }, provenance: "native",
  event: { type: "timeline_appended", task_id: taskId, item: { seq, at: seq, run_id: null, item: { kind: "note", text: `Step ${seq}` } } },
});

let link: HeldLink;
let session: HostSession;
let snap: Snapshot;
beforeEach(async () => {
  localStorage.clear();
  link = new HeldLink();
  session = new HostSession(profile.profileId, link);
  snap = { ...snapshot(), journal_head: 10 };
  session.start();
  link.emit({ type: "authenticated", challenge: { selected: { major: 1, minor: 0 }, host_id: snap.host.host_id, boot_epoch: fx.ids.boot, server_nonce: "fixture" },
    auth: { host_id: snap.host.host_id, device_id: profile.deviceId, grants: allGrants, capabilities: fx.capabilities(), companion_version: "test", journal_head: 10 } });
  link.answer({ kind: "snapshot", ...snap });
  await flush();
  link.emit({ type: "frame", frame: { type: "caught_up", head: 10 } });
});
afterEach(() => {
  cleanup();
  session.stop();
});

it("opening a display keeps journal events that were queued behind its snapshot", async () => {
  const app: App = { platform: platform().value, profile, profiles: [profile], session, ui: new UiStore(profile.profileId),
    switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  const picture = { current: { at: 0, revision: 1, connected: true, error: null } as Picture };
  renderHook(() => useControl(fx.ids.tgHdmi, picture, true), {
    wrapper: ({ children }: { children: ReactNode }) => createElement(AppProvider, { app, children }),
  });
  const task = snap.tasks[0]!;
  // The computer journals seq 11 and queues it; the display's snapshot
  // already counts it and is written first.
  link.answer({ kind: "snapshot", ...snap, journal_head: 11 });
  await flush();
  act(() => link.emit({ type: "frame", frame: { type: "event", ...note(11, task.task_id) } }));
  expect(session.state.get().timelines[task.task_id]?.items.map((i) => i.seq)).toEqual([11]);
  expect(session.state.get().journalHead).toBe(11);
});

it("a refreshed snapshot replaces what it carries but never skips journal events", () => {
  const task = snap.tasks[0]!;
  const state = session.state.get();
  const renamed = { ...snap, journal_head: 12, tasks: snap.tasks.map((t) => t.task_id === task.task_id ? { ...t, title: "Renamed" } : t) };
  const refreshed = refreshFromSnapshot(state, renamed, 5);
  expect(refreshed.tasks[task.task_id]?.title).toBe("Renamed");
  expect(refreshed.journalHead).toBe(10);
  // An older answer than what the phone has applied changes nothing.
  expect(refreshFromSnapshot(refreshed, { ...snap, journal_head: 9 }, 6)).toBe(refreshed);
});
