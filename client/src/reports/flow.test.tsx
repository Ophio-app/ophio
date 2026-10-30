// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { HostSession } from "../host/session";
import { createSimPlatform } from "../dev/sim/platform";
import { reports } from "../diagnostics/reports";
import { RouteProvider } from "../shell/Screen";
import { YourReports } from "./YourReports";
import { ReportNeedsYou, ReportQuestionCard } from "./Questions";
import { computerReports } from "./store";
import Settings from "../screens/settings/Settings";
import * as toastModule from "../design/Toast";
import { notificationSettings } from "../platform/notificationSettings";
import { ConnectedReports } from "./ReportsOverlay";
import { reportRequest } from "./request";
import { parseOpenRoute, notificationFor } from "../platform/notifications";
import { TOPICS } from "../platform/notificationSettings";

let app: App, detach: () => void;
const advance = (ms = 100) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const route = { screen: "settings" } as const;
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 1, active: true, route }}>{children}</RouteProvider></AppProvider>;
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear(); reportRequest.set(null);
  reports.set(s => ({ ...s, queued: [], direct: [], sent: {}, tasks: {}, error: null, settings: { sending: true, ring: "everyone", revision: 0 } }));
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  Element.prototype.scrollTo ??= () => {};
  const platform = createSimPlatform(), profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: vi.fn() };
  session.start(); detach = reports.attach(session, app.ui); await advance(900);
});
afterEach(() => { cleanup(); detach(); app.session.stop(); window.__sim!.host.dispose(); reportRequest.set(null); vi.useRealTimers(); vi.restoreAllMocks(); });
async function submitted() {
  const preview = vi.spyOn(app.session, "query").mockImplementation(q => window.__sim!.host.query(q) as never);
  const draft = await reports.startReport({ type: "settings" }, { screen: "/work", task: window.__sim!.host.tasks[0]!.task_id });
  preview.mockRestore();
  reports.setWords(draft, "The task did not start."); reports.send(draft); await advance(500); return draft;
}
it("Settings opens the common report sheet and lists Your reports", async () => {
  render(<Settings route={route} />, { wrapper }); await advance();
  fireEvent.click(screen.getByRole("button", { name: /^Report a problem/ }));
  expect(reportRequest.get()?.origin).toEqual({ type: "settings" });
  expect(screen.getByRole("button", { name: /^Your reports/ })).toBeTruthy();
});
it("lists reports, reviews the exact sent bundle, confirms withdrawal, and saves settings against their revision", async () => {
  const d = await submitted();
  const { unmount } = render(<YourReports />, { wrapper }); await advance();
  expect(screen.getByRole("button", { name: new RegExp(`Report ${d.reference}`) }).textContent).toContain("Work");
  fireEvent.click(screen.getByLabelText("Hear about fixes early"));
  expect((screen.getByLabelText("Hear about fixes early") as HTMLInputElement).checked).toBe(true);
  await advance(300); expect(window.__sim!.host.reports.view.settings).toMatchObject({ ring: "early", revision: 1 });
  unmount(); render(<YourReports reportId={d.id} />, { wrapper }); await advance();
  fireEvent.click(screen.getByRole("button", { name: "See what was sent" }));
  expect(screen.getByText("The task did not start.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Withdraw" }));
  expect(screen.getByText(/Any copy sent to the project is deleted with its attachments/)).toBeTruthy();
  expect(window.__sim!.host.reports.details.has(d.id)).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Withdraw report" })); await advance(300);
  expect(screen.getByRole("status").textContent).toBe("This report was withdrawn. Removal of any sent copy may still be pending.");
  expect(window.__sim!.host.reports.details.has(d.id)).toBe(false);
});
it("answers a Needs-you report question on the tap, then settles, and offers Update now after a fix", async () => {
  const d = await submitted(); await advance(3500);
  const { unmount } = render(<ReportNeedsYou />, { wrapper }); await advance(200);
  fireEvent.click(screen.getByRole("button", { name: "Yes" }));
  expect(screen.getByText("Answered: Yes")).toBeTruthy();
  await advance(1600);
  unmount(); render(<YourReports reportId={d.id} />, { wrapper }); await advance(300);
  expect(screen.getByText("Fixed in this update")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Update now" })).toBeTruthy();
});
it("restores the choices and explains a refused answer quietly", async () => {
  await submitted(); await advance(3500);
  window.__sim!.host.reports.failNextAnswer = true;
  render(<ReportNeedsYou />, { wrapper }); await advance(200);
  fireEvent.click(screen.getByRole("button", { name: "No" }));
  expect(screen.getByText("Answered: No")).toBeTruthy(); await advance(300);
  expect(screen.getByRole("button", { name: "No" })).toBeTruthy();
  expect(screen.getByText("Already handled somewhere else.")).toBeTruthy();
});
it("Allow opens a reviewed follow-up before answering", async () => {
  const d = await submitted();
  const answer = vi.spyOn(reports, "answer").mockResolvedValue();
  render(<ReportQuestionCard reportId={d.id} question={{ question_id: "q", asked_at: 1, template: { template: "may_we_see", part: "task_transcript" } }} />, { wrapper });
  fireEvent.click(screen.getByRole("button", { name: "Allow" }));
  expect(reportRequest.get()).toMatchObject({ origin: { type: "follow_up", report_id: d.id }, extra: { type: "task_transcript", task_id: d.taskId } });
  expect(answer).not.toHaveBeenCalled();
  act(() => reportRequest.get()?.onSent?.()); expect(answer).toHaveBeenCalledOnce();
});
it("gates remote requests without reports_v1 while keeping the local path and settings", async () => {
  app.session.state.set(s => ({ ...s, capabilities: { ...s.capabilities!, features: [] } }));
  const query = vi.spyOn(app.session, "query");
  render(<YourReports />, { wrapper }); await advance(200);
  expect(query).not.toHaveBeenCalled();
  fireEvent.click(screen.getByLabelText("Send reports"));
  expect(reports.get().settings.sending).toBe(false);
  expect(computerReports(app.session).supported()).toBe(false);
});
it("shows halted-build instructions, and report alerts open the report", async () => {
  window.__sim!.host.reports.view.halted = { build: "sim", back_to: "previous" };
  render(<YourReports />, { wrapper }); await advance(200);
  expect(screen.getByText(/ophio update --rollback/)).toBeTruthy();
  expect(TOPICS.find(t => t.topic === "reports")?.label).toBe("Problem reports");
  expect(parseOpenRoute({ kind: "report", profileId: "p", reportId: "r" })).toEqual({ kind: "report", profileId: "p", reportId: "r" });
  const note = notificationFor({ attention_id: "a", report_id: "r", kind: "question", created_at: 1, resolved_at: null, task_id: null, run_id: null, approval_id: null, title: "", body: null, priority: 1 }, app.session.state.get(), app.profile, "minimal");
  expect(note?.route).toEqual({ kind: "report", profileId: app.profile.profileId, reportId: "r" });
});

it("alerts for direct report progress and respects the Reports topic switch", async () => {
  const d = await submitted();
  const summary = window.__sim!.host.reports.view.reports.find(r => r.report_id === d.id)!;
  reports.set(s => ({ ...s, direct: [summary] }));
  const toast = vi.spyOn(toastModule, "toast").mockImplementation(() => {});
  render(<ConnectedReports />, { wrapper }); await advance(200);
  act(() => reports.set(s => ({ ...s, direct: [{ ...summary, history: [...summary.history, { at: Date.now(), status: { state: "reproduced" } }] }] })));
  expect(toast).toHaveBeenCalledWith(expect.stringContaining("Reproduced"), expect.objectContaining({ action: expect.objectContaining({ label: "Open report" }) }));
  const settings = notificationSettings(app.session);
  act(() => settings.set({ view: { off: ["reports"], projects: [], bundle: false, revision: 1 }, error: null }));
  act(() => reports.set(s => ({ ...s, direct: [{ ...summary, history: [...summary.history, { at: Date.now(), status: { state: "fixed", build: "next" } }] }] })));
  expect(toast).toHaveBeenCalledOnce();
});
