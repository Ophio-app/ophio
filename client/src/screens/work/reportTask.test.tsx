// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, useUi, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { AttentionItem } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import Attention from "./AttentionScreen";
import Receipt from "./Receipt";

const REPORT = "0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b";
const REFERENCE = "Z4YPKB";
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
function Front() {
  const s = useUi((s) => s);
  const route = s.stacks[s.tab].at(-1);
  if (!route) throw new Error("Missing route");
  return <RouteProvider value={{ tab: s.tab, route, depth: s.stacks[s.tab].length - 1, active: true }}>
    {route.screen === "attention" ? <Attention route={route} /> : route.screen === "receipt" ? <Receipt route={route} /> : null}
  </RouteProvider>;
}
function open(route: Route) { app.ui.push(route); render(<AppProvider app={app}><Front /></AppProvider>); }

/** A report in the owner's inbox, on the computer and the phone, from a
 * computer advertising `features`. */
function inbox(features: string[]) {
  const host = window.__sim?.host;
  const fixture = host?.attention.find((a) => a.attention_id === ids.atReady);
  if (!host || !fixture) throw new Error("Missing fixture alert");
  const item: AttentionItem = { ...fixture, kind: "waiting_for_you", task_id: null, run_id: null, report_id: REPORT, topic: "reports", resolved_at: null,
    title: `New problem report ${REFERENCE}`, body: "They wrote: “I opened a notification and got an error”\nReported once · status: new" };
  host.attention = host.attention.map((a) => (a.attention_id === item.attention_id ? item : a));
  act(() => app.session.state.set((s) => {
    if (!s.capabilities) throw new Error("Missing capabilities");
    return { ...s, capabilities: { ...s.capabilities, features: [...s.capabilities.features, ...features] }, attention: { ...s.attention, [item.attention_id]: item } };
  }));
}
const sent = () => Object.values(app.session.commands.get()).map((c) => c.body);
const BUTTON = "Have an agent look into it";

it("one tap has an agent plan a fix in a new task, which the report then links to", async () => {
  inbox(["report_inbox_v1", "report_task_v1"]);
  const query = vi.spyOn(app.session, "query");
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  expect(screen.getByText(/It can’t change anything until you read its plan and choose Carry out this plan/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Look into it" })).toBeNull();
  expect(sent()).toEqual([]);

  // The simulated computer holds no copy of Ophio's source, so the person
  // chooses the project once.
  fireEvent.click(screen.getByRole("button", { name: BUTTON })); await advance();
  expect(query.mock.calls.map(([q]) => q.kind)).toContain("report_task_project");
  const picker = screen.getByRole("dialog", { name: "Which project holds Ophio’s source?" });
  expect(sent()).toEqual([]);
  fireEvent.click(within(picker).getByRole("button", { name: /tidewater-api/ })); await advance(); await advance();
  expect(sent()).toEqual([{ command: "start_report_task", report_id: REPORT, project_id: ids.pTide }]);

  const made = Object.values(app.session.state.get().tasks).find((t) => t.report?.report_id === REPORT);
  expect(made).toMatchObject({ project_id: ids.pTide, mode: "plan", title: `Problem report ${REFERENCE}`, report: { report_id: REPORT, reference: REFERENCE } });
  // The new task opens.
  expect(app.ui.get().tab).toBe("talk");
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(made?.task_id);

  // Back on the report: who is on it, and no second task to start.
  act(() => app.ui.push({ screen: "attention", attentionId: ids.atReady }, "work")); await advance();
  expect(screen.getByText("An agent is looking into this")).toBeTruthy();
  expect(screen.queryByRole("button", { name: BUTTON })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: `Open “Problem report ${REFERENCE}”` })); await advance();
  expect(app.ui.get().tab).toBe("talk");
  expect(sent()).toHaveLength(1);
});

it("next time the computer knows the project, so one tap starts it", async () => {
  inbox(["report_inbox_v1", "report_task_v1"]);
  if (window.__sim) window.__sim.host.reportTaskProject = ids.pTide;
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  fireEvent.click(screen.getByRole("button", { name: BUTTON })); await advance(); await advance();
  expect(screen.queryByRole("dialog")).toBeNull();
  // Only a choice the person makes is sent; the computer decides otherwise.
  expect(sent()).toEqual([{ command: "start_report_task", report_id: REPORT }]);
  expect(Object.values(app.session.state.get().tasks).some((t) => t.report?.report_id === REPORT && t.mode === "plan")).toBe(true);
});

it("leaves the contained fixer's Look into it exactly as it was, and offers nothing on older computers", async () => {
  inbox(["report_inbox_v1", "report_look_into_v1", "report_task_v1"]);
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  expect(screen.getByRole("button", { name: "Look into it" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: BUTTON })).toBeNull();
  cleanup();
  act(() => app.session.state.set((s) => s.capabilities ? { ...s, capabilities: { ...s.capabilities, features: s.capabilities.features.filter((f) => f !== "report_look_into_v1" && f !== "report_task_v1") } } : s));
  render(<AppProvider app={app}><Front /></AppProvider>); await advance();
  expect(screen.queryByRole("button", { name: "Look into it" })).toBeNull();
  expect(screen.queryByRole("button", { name: BUTTON })).toBeNull();
  // A phone without permission to start tasks can't start one either.
  act(() => app.session.state.set((s) => s.capabilities ? { ...s, capabilities: { ...s.capabilities, features: [...s.capabilities.features, "report_task_v1"] }, grants: s.grants && { ...s.grants, tasks: false } } : s));
  expect((screen.getByRole("button", { name: BUTTON }) as HTMLButtonElement).disabled).toBe(true);
  expect(sent()).toEqual([]);
});

it("a task's receipt says which report it looks into, and opens it", async () => {
  inbox(["report_inbox_v1", "report_task_v1"]);
  act(() => app.session.state.set((s) => {
    const task = s.tasks[ids.tHeaders];
    if (!task) throw new Error("Missing fixture task");
    return { ...s, tasks: { ...s.tasks, [task.task_id]: { ...task, report: { report_id: REPORT, reference: REFERENCE } } } };
  }));
  open({ screen: "receipt", taskId: ids.tHeaders }); await advance();
  expect(screen.getByText(`Looking into problem report ${REFERENCE}`)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Open report" })); await advance();
  expect(app.ui.route()).toEqual({ screen: "attention", attentionId: ids.atReady });
});

it("Use another project lets the person change the project the computer remembers", async () => {
  inbox(["report_inbox_v1", "report_task_v1"]);
  if (window.__sim) window.__sim.host.reportTaskProject = ids.pTide;
  const query = vi.spyOn(app.session, "query");
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  fireEvent.click(screen.getByRole("button", { name: "Use another project" })); await advance();
  // No asking the computer first: the person is choosing.
  expect(query.mock.calls.some(([q]) => q.kind === "report_task_project")).toBe(false);
  const picker = screen.getByRole("dialog", { name: "Which project holds Ophio’s source?" });
  fireEvent.click(within(picker).getByRole("button", { name: /garden-planner/ })); await advance(); await advance();
  expect(sent()).toEqual([{ command: "start_report_task", report_id: REPORT, project_id: ids.pGarden }]);
  expect(window.__sim?.host.reportTaskProject).toBe(ids.pGarden);
});
