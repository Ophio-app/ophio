// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { TABS, UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { ids } from "../dev/sim/scenario";
import { HostSession } from "../host/session";
import { applyLive } from "../state/projection";
import { Shell } from "./Shell";

const pause = ((wait) => (ms: number) => new Promise((done) => wait(done, ms)))(globalThis.setTimeout.bind(globalThis));
let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it("walks no runs or approvals while only streamed text and the time last heard change", async () => {
  const { session, ui } = app;
  const task = session.state.get().tasks[ids.tDash]!;
  render(<AppProvider app={app}><Shell /></AppProvider>);
  const settle = async () => { for (let i = 0; i < 10; i++) { await act(async () => { await vi.advanceTimersByTimeAsync(100); }); await act(() => pause(10)); } };
  for (const tab of TABS) { act(() => ui.selectTab(tab)); await settle(); }
  act(() => { ui.selectTab("talk"); ui.selectTask(task.project_id, task.task_id); });
  for (let i = 0; i < 10 && !document.querySelector(".talk-log"); i++) await settle();
  expect(document.querySelector(".talk-log")).toBeTruthy();

  const reads = { runs: 0, approvals: 0 };
  const counted = <T extends object>(record: T, name: keyof typeof reads) => new Proxy(record, { ownKeys: (target) => { reads[name]++; return Reflect.ownKeys(target); } });
  act(() => session.state.set((s) => ({ ...s, runs: counted(s.runs, "runs"), approvals: counted(s.approvals, "approvals") })));
  reads.runs = reads.approvals = 0;
  for (let i = 0; i < 3; i++) {
    vi.setSystemTime(Date.now() + 50);
    act(() => session.state.set((s) => applyLive(s, { kind: "agent_text_delta", task_id: task.task_id, run_id: task.current_run!, message_key: "m", text: `word${i} ` }, Date.now())));
  }
  expect(reads).toEqual({ runs: 0, approvals: 0 });
}, 60_000);
