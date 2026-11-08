// SPDX-License-Identifier: Apache-2.0
//
// A problem report about another computer, as from an error on one of All's
// rows, is filed through that computer and describes it, not the one on screen.
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act } from "@testing-library/react";
import { UiStore } from "../app/ui";
import { HostSession } from "../host/session";
import { createSimPlatform } from "../dev/sim/platform";
import { reports } from "../diagnostics/reports";

let studio: HostSession, spare: HostSession, off: (() => void)[] = [];
const advance = (ms = 100) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
/** The real event loop's next turn, kept before the clock is faked: hashing
 * a report runs on real time, outside the fake clock. */
const realTurn: (callback: () => void) => unknown = (globalThis as unknown as { setImmediate: (callback: () => void) => unknown }).setImmediate;
const turn = () => new Promise<void>((resolve) => realTurn(() => resolve()));
/** Lets the simulated computers answer while `work` runs: the fake clock
 * moves only while it waits, and the real loop turns between steps, so it
 * ends when the work does, however slow the machine. */
async function settled<T>(work: Promise<T>): Promise<T> {
  let done = false;
  void work.finally(() => { done = true; }).catch(() => {});
  while (!done) { await advance(50); await turn(); }
  return work;
}
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  reports.set((s) => ({ ...s, queued: [], direct: [], sent: {}, tasks: {}, error: null, settings: { sending: true, ring: "everyone", revision: 0 } }));
  window.history.replaceState(null, "", "/?three-computers");
  const platform = createSimPlatform();
  studio = new HostSession("sim-studio", platform.control("sim-studio"));
  spare = new HostSession("sim-spare", platform.control("sim-spare"));
  studio.start(); spare.start();
  await advance(900);
  off = [reports.attach(studio, new UiStore("sim-studio")), reports.setSessions((id) => (id === "sim-spare" ? spare : undefined))];
});
afterEach(() => {
  for (const f of off) f();
  studio.stop(); spare.stop(); window.__sim?.host.dispose();
  window.history.replaceState(null, "", "/");
  vi.useRealTimers(); vi.restoreAllMocks();
});

it("files a report about another computer through it, and reads that computer for it", async () => {
  const there = vi.spyOn(spare, "query"), here = vi.spyOn(studio, "query");
  const started = reports.startReport({ type: "error" }, { screen: "/work", command: "resolve_approval", error: { code: "expired", message: "That request expired." }, profileId: "sim-spare" });
  const draft = await settled(started);
  expect(draft.profileId).toBe("sim-spare");
  expect(there.mock.calls.some(([q]) => q.kind === "report_preview")).toBe(true);
  expect(here.mock.calls.some(([q]) => q.kind === "report_preview")).toBe(false);
  // The computer's name for itself stays out of the report.
  expect(JSON.stringify(draft.bundle)).not.toContain("sim-spare");
  const sent = vi.spyOn(spare, "command");
  reports.send(draft);
  await advance(900);
  expect(sent.mock.calls.some(([c]) => c.command === "submit_report")).toBe(true);
});

it("keeps a report about the computer on screen going through it", async () => {
  const started = reports.startReport({ type: "settings" }, { screen: "/work" });
  const draft = await settled(started);
  expect(draft.profileId).toBe("sim-studio");
});
