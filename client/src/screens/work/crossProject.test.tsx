// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";

import { ApprovalQuickActions } from "../talk/ApprovalQuickActions";
import { AttentionList } from "./shared";
import { Transcript } from "../talk/Transcript";
import Receipt from "./Receipt";
import Work from "./Work";
let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing fixture profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it("locates other projects’ decisions from Needs you and the project switcher", async () => {
  app.ui.selectProject(ids.pNotes);
  const route = { screen: "work" } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><Work route={route} /></RouteProvider></AppProvider>);
  await advance();
  fireEvent.click(screen.getByRole("button", { name: /in tidewater-api.*Approval needed/ }));
  expect(app.ui.get().projectId).toBe(ids.pTide);
  // The header's project name is the one switcher.
  fireEvent.click(within(screen.getByRole("heading", { level: 1 })).getByRole("button"));
  expect(screen.getByRole("button", { name: /tidewater-api.*need you/ })).toBeTruthy();
});

it("does not carry an accepted decision or working spinner into a re-presented approval", async () => {
  const route = { screen: "work" } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><AttentionList projectId={ids.pTide} /></RouteProvider></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: /Fix the flaky retry test.*Approval needed/ }));
  const old = app.session.state.get().approvals[ids.aTest]!;
  fireEvent.click(screen.getAllByRole("button", { name: "Allow" }).at(-1)!);
  await advance();
  expect(screen.getByText(/Sent to studio/)).toBeTruthy();
  act(() => {
    const approval = { ...old, requested_at: old.requested_at + 1 };
    window.__sim!.host.emit({ type: "approval_upserted", approval });
  });
  expect(screen.queryByText(/Sent to studio/)).toBeNull();
  expect(screen.getByRole("button", { name: "Allow" })).toBeTruthy();
  cleanup();
  const task = app.session.state.get().tasks[old.task_id]!;
  const { container } = render(<AppProvider app={app}><Transcript task={task} agent="Claude Code" /></AppProvider>);
  await advance();
  expect(screen.getByText("Waiting for your approval")).toBeTruthy();
  expect(container.querySelector(".talk-working .stateline__spinner")).toBeNull();
});

it("flags a reported model or effort mismatch beside Requested and Actual", async () => {
  const route = { screen: "receipt", taskId: ids.tHeaders } as const;
  const run = app.session.state.get().runs[ids.rHeaders]!;
  act(() => window.__sim!.host.emit({ type: "run_upserted", run: { ...run, requested_model: "claude-sonnet-5", actual_model: "claude-opus-5-5", requested_effort: "medium", actual_effort: "high" } }));
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 1, active: true }}><Receipt route={route} /></RouteProvider></AppProvider>);
  await advance();
  expect(screen.getByText("Different from requested")).toBeTruthy();
  act(() => window.__sim!.host.emit({ type: "run_upserted", run: { ...run, actual_model: null, actual_effort: null } }));
  expect(screen.queryByText("Different from requested")).toBeNull();
});

it("makes the shared inline denial primary and records deny, not allow", async () => {
  render(<AppProvider app={app}><ApprovalQuickActions approvalId={ids.aTest} /></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Deny…" }));
  const deny = screen.getByRole("button", { name: "Deny request" });
  expect(deny.classList.contains("btn--primary")).toBe(true);
  expect(screen.getByRole("button", { name: "Allow" }).classList.contains("btn--quiet")).toBe(true);
  fireEvent.change(screen.getByLabelText("Reason (optional)"), { target: { value: "Not yet" } });
  fireEvent.click(deny);
  await advance(900);
  expect(app.session.state.get().approvals[ids.aTest]?.state).toBe("denied");
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "resolve_approval" && c.body.decision.decision === "deny" && c.body.decision.reason === "Not yet")).toBe(true);
});

it("an inline Allow leaves focus where the list begins, not on the page", async () => {
  render(<AppProvider app={app}><section className="route"><h2 className="section__title">Waiting for you</h2><ApprovalQuickActions approvalId={ids.aTest} /></section></AppProvider>);
  const allow = screen.getByRole("button", { name: "Allow" });
  allow.focus();
  fireEvent.click(allow);
  await advance(900);
  expect(app.session.state.get().approvals[ids.aTest]?.state).toBe("allowed");
  expect(document.activeElement).toBe(screen.getByRole("heading", { name: "Waiting for you" }));
});
