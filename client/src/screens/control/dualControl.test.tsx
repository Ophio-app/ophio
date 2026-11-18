// SPDX-License-Identifier: Apache-2.0
// Two real control hooks share one real computer session. Only their incoming
// pictures are synthetic; snapshots, leases, commands and effects use the sim.
import { StrictMode } from "react";
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { useControl } from "./useControl";
import type { Picture } from "./useMedia";

let app: App;
let targetId: string;
let run = 0;
const epoch = Date.now();
const controls = new Map<string, ReturnType<typeof useControl>>();
const pictures = new Map<string, { current: Picture }>();
const advance = (ms = 0) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
function Probe({ side, visible }: { side: string; visible: boolean }) {
  controls.set(side, useControl(targetId, pictures.get(side)!, visible));
  return null;
}
function Pair({ left = true, right = true, leftVisible = true, rightVisible = true }: {
  left?: boolean; right?: boolean; leftVisible?: boolean; rightVisible?: boolean;
}) {
  return <StrictMode><AppProvider app={app}>
    {left && <Probe key="left" side="left" visible={leftVisible} />}
    {right && <Probe key="right" side="right" visible={rightVisible} />}
  </AppProvider></StrictMode>;
}
const commands = (name: string) => Object.values(app.session.commands.get()).filter(c => c.body.command === name);
const view = () => app.session.state.get().control[targetId];
async function mount() {
  const mounted = render(<Pair />);
  await advance(100);
  expect(controls.get("left")!.reconciled).toBe(true);
  expect(controls.get("right")!.reconciled).toBe(true);
  return mounted;
}
async function humanControl() {
  targetId = ids.tgHdmi;
  const mounted = await mount();
  act(() => { controls.get("left")!.arm(); controls.get("right")!.arm(); });
  const target = app.session.state.get().targets.find(t => t.target_id === targetId)!;
  const request = app.session.command({ command: "request_human_lease", target_id: targetId,
    geometry_revision: target.geometry.revision }, { label: "Requesting control" });
  await advance(800); await request; await advance(100);
  expect(controls.get("left")!.enabled).toBe(true);
  expect(controls.get("right")!.enabled).toBe(true);
  return mounted;
}
beforeEach(async () => {
  // The shared real UI clock survives tests. Do not move Date backwards
  // after a renewal test has advanced it by several seconds.
  vi.useFakeTimers({ now: epoch + ++run * 60_000 });
  localStorage.clear(); controls.clear(); pictures.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform(), profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId),
    switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
  targetId = ids.tgDp1;
  for (const side of ["left", "right"]) pictures.set(side, { current: {
    // Fresh frames keep arriving unless a test explicitly disconnects a view.
    get at() { return Date.now(); },
    get revision() { return session.state.get().targets.find(t => t.target_id === targetId)!.geometry.revision; },
    connected: true, error: null,
  } });
});
afterEach(async () => {
  cleanup(); await advance();
  app.session.stop(); window.__sim?.host.dispose();
  app.ui.retire();
  Reflect.deleteProperty(document, "visibilityState");
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("reconciles two simultaneously mounted views with one shared snapshot, including StrictMode remounts", async () => {
  const query = vi.spyOn(app.session, "query");
  await mount();
  expect(query.mock.calls.filter(([request]) => request.kind === "snapshot")).toHaveLength(1);
  expect(view()?.input_state).toBe("agent");
  expect(commands("pause_input")).toHaveLength(0);
  expect(commands("request_human_lease")).toHaveLength(0);
});

it("renews a supervised lease once per interval and transfers renewal responsibility when a view closes", async () => {
  const renew = vi.spyOn(app.session, "renewLease");
  const mounted = await mount();
  const lease = view()!.lease!;
  const interval = Math.max(50, Math.floor(lease.renewal_window_ms / 3));
  await advance(interval + 1);
  expect(renew).toHaveBeenCalledTimes(1);
  expect(renew).toHaveBeenLastCalledWith(lease.lease_id, lease.generation);
  await advance(interval);
  expect(renew).toHaveBeenCalledTimes(2);
  mounted.rerender(<Pair left={false} />); await advance();
  await advance(interval);
  expect(renew).toHaveBeenCalledTimes(3);
  expect(commands("pause_input")).toHaveLength(0);
  expect(view()?.input_state).toBe("agent");
});

it.each(["left", "right"] as const)("closing the %s supervised view keeps the other live; the last close pauses once", async closing => {
  const mounted = await mount();
  mounted.rerender(<Pair left={closing !== "left"} right={closing !== "right"} />);
  await advance(800);
  expect(commands("pause_input")).toHaveLength(0);
  expect(view()?.input_state).toBe("agent");
  mounted.unmount(); await advance(800);
  expect(commands("pause_input")).toHaveLength(1);
  expect(view()?.input_state).toBe("paused");
});

it("hiding one route preserves supervision until the last visible route is hidden", async () => {
  const mounted = await mount();
  mounted.rerender(<Pair leftVisible={false} />); await advance(800);
  expect(commands("pause_input")).toHaveLength(0);
  expect(view()?.input_state).toBe("agent");
  mounted.rerender(<Pair leftVisible={false} rightVisible={false} />); await advance(800);
  expect(commands("pause_input")).toHaveLength(1);
  expect(view()?.input_state).toBe("paused");
});

it.each(["pagehide", "visibilitychange"] as const)("%s pauses supervision globally even while both views remain mounted", async event => {
  const renew = vi.spyOn(app.session, "renewLease");
  await mount();
  if (event === "visibilitychange") {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    fireEvent(document, new Event(event));
  } else fireEvent(window, new Event(event));
  await advance(800);
  expect(commands("pause_input")).toHaveLength(1);
  expect(view()?.input_state).toBe("paused");
  renew.mockClear(); await advance(2000);
  expect(renew).not.toHaveBeenCalled();
});

it("closing one armed human view preserves the other's input; closing the last releases it once", async () => {
  const mounted = await humanControl();
  mounted.rerender(<Pair left={false} />); await advance(800);
  expect(commands("release_lease")).toHaveLength(0);
  expect(controls.get("right")!.enabled).toBe(true);
  act(() => { expect(controls.get("right")!.send({ action: "key_press", key: "shift", pressed: true })).toBe(true); });
  mounted.unmount(); await advance(800);
  expect(commands("release_lease")).toHaveLength(1);
  expect(view()?.lease?.state).toBe("revoked");
  expect(window.__sim!.control.inputs().map(input => input.action)).toEqual([
    { action: "key_press", key: "shift", pressed: true },
    { action: "key_press", key: "shift", pressed: false },
    { action: "release_all" },
  ]);
});

it.each(["pagehide", "visibilitychange"] as const)("%s releases a shared human lease rather than preserving another mounted view", async event => {
  await humanControl();
  if (event === "visibilitychange") {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    fireEvent(document, new Event(event));
  } else fireEvent(window, new Event(event));
  await advance(800);
  expect(commands("release_lease")).toHaveLength(1);
  expect(view()?.lease?.state).toBe("revoked");
  expect(controls.get("left")!.enabled).toBe(false);
  expect(controls.get("right")!.enabled).toBe(false);
});

it("an explicit Leave on one side relinquishes the shared computer lease and blocks input on both", async () => {
  await humanControl();
  act(() => controls.get("left")!.leave()); await advance(800);
  expect(commands("release_lease")).toHaveLength(1);
  expect(view()?.lease?.state).toBe("revoked");
  for (const side of ["left", "right"]) {
    expect(controls.get(side)!.enabled).toBe(false);
    act(() => { expect(controls.get(side)!.send({ action: "text", text: "must not send" })).toBe(false); });
  }
  expect(window.__sim!.control.inputs().every(input => input.action.action === "release_all")).toBe(true);
});

it("a stale supervised picture still pauses globally instead of borrowing freshness from the other view", async () => {
  await mount();
  pictures.get("left")!.current.connected = false;
  await advance(1200);
  expect(commands("pause_input")).toHaveLength(1);
  expect(view()?.input_state).toBe("paused");
});
