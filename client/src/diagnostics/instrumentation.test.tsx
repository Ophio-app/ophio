// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { Notice } from "../design/Notice";
import { toast } from "../design/Toast";
import { UiStore } from "../app/ui";
import { HostSession, type PendingCommand } from "../host/session";
import type { ControlLink } from "../host/link";
import { trail } from "./trail";

beforeEach(() => { localStorage.clear(); vi.useFakeTimers(); vi.setSystemTime(Date.now() + 20 * 60_000); });
afterEach(() => { cleanup(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
it("records routes without route arguments and notices without their words", () => {
  const ui = new UiStore("fake"); ui.push({ screen: "folder", rootId: "private-root", path: "/home/sam/private" });
  render(<Notice tone="stop" code="expired">Private error message</Notice>); toast("Private toast message", { code: "permission_denied", tone: "stop" });
  expect(trail.snapshot().map(e => e.event)).toEqual(expect.arrayContaining([{ type: "route", route: "/folder" }, { type: "notice", key: "expired", tone: "error" }, { type: "notice", key: "permission_denied", tone: "error" }]));
  expect(JSON.stringify(trail.snapshot())).not.toMatch(/private|Private/);
});
it("records pending commands, refusals, delayed failures, timing, and all link states", () => {
  const session = new HostSession("fake", {} as ControlLink, Date.now);
  const c: PendingCommand = { id: "fake-command", body: { command: "rename_task", task_id: "private-task", title: "Private title" }, label: "Private label", issuedAt: Date.now(), phase: "sending", receipt: null, error: null };
  session.commands.set({ [c.id]: c }); vi.advanceTimersByTime(123);
  session.commands.set({ [c.id]: { ...c, phase: "rejected", error: { code: "permission_denied", message: "Private refusal" } } });
  session.state.set(s => ({ ...s, commandStages: { [c.id]: { stage: "failed", detail: "Private failure", error: { code: "internal", message: "Private" }, at: Date.now() } } }));
  for (const state of [{ state: "connecting", attempt: 1 }, { state: "online", since: 1, route: "relay" }, { state: "online", since: 2, route: "direct" }, { state: "idle" }] as const) session.link.set(state);
  const events = trail.snapshot().map(e => e.event);
  expect(events).toEqual(expect.arrayContaining([{ type: "command", command: "rename_task", outcome: "pending", ms: 0 }, { type: "command", command: "rename_task", outcome: "refused", code: "permission_denied", ms: 123 }, { type: "command", command: "rename_task", outcome: "failed", code: "internal", ms: 123 }]));
  for (const state of ["reconnecting", "relay", "direct", "offline"]) expect(events).toContainEqual({ type: "connection", state });
  expect(JSON.stringify(events)).not.toMatch(/private|Private/);
});
it("records an accepted command once when its effect is observed too", () => {
  const accepted = () => trail.snapshot().filter(e => e.event.type === "command" && e.event.outcome === "accepted").length;
  const before = accepted();
  const session = new HostSession("fake", {} as ControlLink, Date.now);
  const c: PendingCommand = { id: "fake-accepted", body: { command: "rename_task", task_id: "t", title: "x" }, label: "x", issuedAt: Date.now(), phase: "sending", receipt: null, error: null };
  session.commands.set({ [c.id]: c });
  session.commands.set({ [c.id]: { ...c, phase: "accepted" } });
  session.state.set(s => ({ ...s, commandStages: { [c.id]: { stage: "observed", detail: null, at: Date.now() } } }));
  expect(accepted() - before).toBe(1);
});
it("records quiet and loud attention once and skips unrelated projection updates", async () => {
  const { attention } = await import("../dev/sim/scenario");
  const session = new HostSession("fake", {} as ControlLink);
  const items = attention(Date.now()); const item = items[0]!;
  let reads = 0;
  const values = new Proxy({ a: { ...item, attention_id: "a", quiet: false, resolved_at: null }, b: { ...item, attention_id: "b", quiet: true, resolved_at: null } }, { ownKeys: target => { reads++; return Reflect.ownKeys(target); } });
  session.state.set(s => ({ ...s, attention: values }));
  reads = 0; session.state.set(s => ({ ...s, lastHeardAt: 7 })); expect(reads).toBe(0);
  const events = trail.snapshot().filter(e => e.event.type === "alert_received"); expect(events).toHaveLength(2);
  expect(events.map(e => e.event)).toEqual(expect.arrayContaining([expect.objectContaining({ quiet: true }), expect.objectContaining({ quiet: false })]));
});
it("keeps one line in the trail for a burst of reconnects, with why the link went offline", () => {
  // From a problem report: hundreds of "offline" lines in a row, then an offline
  // notice for every try, crowded out everything before them.
  const session = new HostSession("fake", {} as ControlLink, Date.now);
  const before = trail.snapshot().length;
  const offline = { state: "offline", since: 1, reason: "The computer closed the connection at /home/sam/x.", retryAt: null } as const;
  for (let i = 0; i < 50; i++) session.link.set({ ...offline, since: i });
  for (let i = 0; i < 20; i++) {
    session.link.set({ state: "connecting", attempt: i + 1 });
    session.link.set({ state: "authenticating" });
    session.link.set({ state: "online", since: i, route: "direct" });
    session.link.set({ ...offline, since: i });
    const { unmount } = render(<Notice tone="unknown" code="storm_check">Connect to the computer.</Notice>);
    unmount();
    vi.advanceTimersByTime(50);
  }
  const events = trail.snapshot().slice(before).map(e => e.event);
  // The first run of offline states is one line, not fifty.
  expect(events[0]).toEqual({ type: "connection", state: "offline" });
  expect(events[1]).not.toEqual({ type: "connection", state: "offline" });
  expect(events.filter(e => e.type === "notice" && e.key === "storm_check")).toHaveLength(1);
  // Why, once, without the path in it.
  expect(events.filter(e => e.type === "error")).toEqual([{ type: "error", error: { code: "link_offline", message: "The computer closed the connection at <path>." } }]);
  // A notice coming back after a while is recorded again.
  vi.advanceTimersByTime(10_000);
  render(<Notice tone="unknown" code="storm_check">Connect to the computer.</Notice>);
  expect(trail.snapshot().filter(e => e.event.type === "notice" && e.event.key === "storm_check")).toHaveLength(2);
});

it("tells notices that share a key apart by their words, without recording the words", () => {
  const count = () => trail.snapshot().filter(e => e.event.type === "notice" && e.event.key === "toast").length;
  const before = count();
  // Toasts without a code, as almost all are.
  toast("Private: saved the draft"); toast("Private: couldn't reach the computer", { tone: "stop" }); toast("Private: renamed");
  toast("Private: saved the draft");
  expect(count() - before).toBe(3);
  const inline = () => trail.snapshot().filter(e => e.event.type === "notice" && e.event.key === "inline_notice").length;
  const inlineBefore = inline();
  render(<><Notice tone="unknown">Private: offline</Notice><Notice tone="unknown">Private: no permission</Notice></>);
  expect(inline() - inlineBefore).toBe(2);
  expect(JSON.stringify(trail.snapshot())).not.toMatch(/Private/);
});
