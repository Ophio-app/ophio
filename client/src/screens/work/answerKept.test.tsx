// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Attention from "./Attention";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("keeps a typed answer when its hand-off to the agent failed and the question is opened again", async () => {
  const host = window.__sim!.host;
  const asked = { ...host.approvals.find(a => a.approval_id === ids.aTest)!, kind: "question" as const, title: "Which staging key should I use?", detail: null, questions: undefined };
  host.approvals = host.approvals.map(a => a.approval_id === asked.approval_id ? asked : a);
  act(() => host.emit({ type: "approval_upserted", approval: asked }));
  await advance();
  const route = { screen: "attention", attentionId: ids.atApproval } as const;
  app.ui.push(route);
  const open = () => render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 1, active: true }}><Attention route={route} /></RouteProvider></AppProvider>);
  const first = open();
  await advance();
  const answer = "Use the read-only key from the team vault, not the admin one.";
  fireEvent.change(screen.getByRole("textbox", { name: "Your answer" }), { target: { value: answer } });
  host.failNextApprovalDelivery = true;
  fireEvent.click(screen.getByRole("button", { name: "Send answer" }));
  await advance(1500);
  // The computer kept the question open at a new revision; the answer never arrived.
  expect(app.session.state.get().approvals[ids.aTest]).toMatchObject({ state: "pending", revision: asked.revision + 1 });
  expect(screen.getByText("The agent did not receive this. It may still be waiting.")).toBeTruthy();
  first.unmount();
  open();
  await advance();
  expect(screen.getByRole("textbox", { name: "Your answer" })).toHaveProperty("value", answer);
});
