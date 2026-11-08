// SPDX-License-Identifier: Apache-2.0
//
// All computers: an agent that isn't signed in on one computer shows under
// Needs you, named with that computer, and Sign in again goes there.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { Computers } from "../../app/computers";
import { UiStore } from "../../app/ui";
import { prefs } from "../../app/prefs";
import { takeStart } from "../../app/signInFeature";
import { createSimPlatform } from "../../dev/sim/platform";
import { RouteProvider } from "../../shell/Screen";
import type { AttentionItem } from "../../protocol/types";
import { AllWork } from "./AllWork";

let app: App;
let release = () => {};
const advance = async (ms = 0) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });

beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  prefs.set((p) => ({ ...p, allProjects: true, warningsOff: true }));
  window.history.replaceState(null, "", "/?three-computers");
  const platform = createSimPlatform();
  const profiles = await platform.profiles();
  const computers = new Computers(platform, profiles);
  release = computers.hold();
  const profile = profiles[0]!;
  const session = computers.session(profile.profileId);
  computers.view(profile.profileId);
  const uis = new Map<string, UiStore>();
  const uiOf = (id: string) => { let ui = uis.get(id); if (!ui) uis.set(id, (ui = new UiStore(id))); return ui; };
  app = { platform, profile, profiles, session, ui: uiOf(profile.profileId), computers, all: true,
    showAll: vi.fn(), openProject: vi.fn(), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {}, forgetOther: vi.fn(),
    uiOf, visit: vi.fn() };
  await advance(1500);
});
afterEach(() => {
  cleanup(); release(); window.__sim?.host.dispose();
  window.history.replaceState(null, "", "/");
  vi.clearAllTimers(); vi.useRealTimers();
});

it("shows each computer's sign-in notice under its name, and Sign in again opens it on that computer", async () => {
  // The spare's computer says Codex isn't signed in there.
  const spare = app.computers!.peek("sim-spare")!;
  const item: AttentionItem = { attention_id: "sign-in-codex-9", kind: "waiting_for_you", topic: "needs_you", account_id: "codex",
    title: "Codex isn’t signed in on spare", body: "New Codex tasks can’t start until it’s signed in again.",
    task_id: null, run_id: null, approval_id: null, created_at: Date.now(), resolved_at: null, priority: 5 };
  act(() => spare.state.set((s) => ({ ...s, attention: { ...s.attention, [item.attention_id]: item } })));
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "work" }, depth: 0, active: true }}><AllWork /></RouteProvider></AppProvider>);
  const notice = screen.getByText("Codex isn’t signed in on spare").closest(".notice") as HTMLElement;
  expect(screen.queryByText("Nothing waiting for you on any computer.")).toBeNull();
  fireEvent.click(within(notice).getByRole("button", { name: "Sign in again" }));
  await advance(0);
  // Nothing moved on the computer on screen; the spare is switched to first.
  expect(app.visit).toHaveBeenCalledWith("sim-spare", expect.any(Function));
  expect(app.ui.get().stacks.work.some((r) => r.screen === "sign_in")).toBe(false);
  const spareUi = app.uiOf!("sim-spare");
  vi.mocked(app.visit!).mock.calls[0]![1](spareUi);
  expect(spareUi.get().stacks[spareUi.get().tab].at(-1)).toEqual({ screen: "sign_in", accountId: "codex" });
  // And it starts there, not on the computer on screen.
  expect(takeStart(app.profile.profileId, "codex")).toBeNull();
  expect(takeStart("sim-spare", "codex")).toBe("always");
});
