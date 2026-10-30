// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Attention from "./Attention";
import { AttentionList } from "./shared";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
  // The computer sends a device without the approvals grant its tasks, timeline
  // and attention, but never the approval records (domain::grants::event).
  act(() => app.session.state.set(s => ({ ...s, grants: { ...s.grants!, approvals: false }, approvals: {} })));
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("an approval alert on a device without the approvals grant says why, not that the request is gone", async () => {
  const route = { screen: "attention", attentionId: ids.atApproval } as const;
  app.ui.push(route);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 1, active: true }}><Attention route={route} /></RouteProvider></AppProvider>);
  await advance();
  expect(app.session.state.get().attention[ids.atApproval]?.resolved_at).toBeNull();
  expect(screen.queryByText("The request this alert refers to is no longer available.")).toBeNull();
  expect(screen.getByText(/approval/i, { selector: ".notice *" })).toBeTruthy();
});

it("the Needs you sheet for that approval says why it can't be answered here", async () => {
  const route = { screen: "work" } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><AttentionList projectId={ids.pTide} /></RouteProvider></AppProvider>);
  fireEvent.click(screen.getByText(/^Approval needed/).closest("button")!);
  const sheet = within(screen.getByRole("dialog"));
  expect(sheet.queryByText("This request isn’t available in the computer’s current state.")).toBeNull();
  expect(sheet.getByText(/approval/i, { selector: ".notice *" })).toBeTruthy();
});
