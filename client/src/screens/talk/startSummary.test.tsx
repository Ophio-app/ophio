// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { freshReview, UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import type { LaunchPolicy } from "../../protocol/types";
import Review from "./Review";
import { startSummary } from "./startSummary";

const base = { project: "tidewater-api", alone: false, agent: "Claude Code", harness: "claude_code" as const, policy: "standard" as LaunchPolicy, mode: "work" as const, placement: "start_now" as const, authorization: "auto_start" as const, paused: false };

it("says where, who, what access and when, in one line", () => {
  expect(startSummary(base)).toEqual({ text: "In tidewater-api · Claude Code · Work, “Ask for approval” · Starts now", risk: null });
  expect(startSummary({ ...base, alone: true }).text).toMatch(/^On its own · /);
  expect(startSummary({ ...base, mode: "plan" }).text).toContain("· Plan, read-only ·");
  expect(startSummary({ ...base, policy: null }).text).toContain("· Work, the agent’s own permission settings ·");
  expect(startSummary({ ...base, harness: "codex", agent: "Codex", policy: "sandboxed" }).text).toContain("· Codex · Work, “Sandbox, no prompts” ·");
});

it("says when work in Next up starts, and whether it asks first", () => {
  const queued = { ...base, placement: "queue" as const };
  expect(startSummary(queued).text).toMatch(/· Next up, starts by itself$/);
  expect(startSummary({ ...queued, authorization: "ask_before_start" }).text).toMatch(/· Next up, asks you first$/);
  expect(startSummary({ ...queued, authorization: "ask_before_start", paused: true }).text).toMatch(/· Next up, paused$/);
  // One that starts by itself resumes a paused Next up.
  expect(startSummary({ ...queued, paused: true }).text).toMatch(/· Next up, starts by itself$/);
});

it("warns when the agent won't ask, and only in Work", () => {
  expect(startSummary({ ...base, policy: "bypass_permissions" }).risk).toBe("It won’t ask before running commands or changing files.");
  expect(startSummary({ ...base, policy: "bypass_permissions", mode: "ask" }).risk).toBeNull();
});

let app: App;
const tick = async () => act(async () => { await vi.advanceTimersByTimeAsync(900); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick();
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

function review() {
  const route: Route = { screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}><Review route={route} /></RouteProvider></AppProvider>);
}
const summary = () => document.querySelector(".review-foot__summary");

it("sits beside Start, in the warning tone when the agent will skip permissions", async () => {
  const host = window.__sim!.host;
  const project = { ...app.session.state.get().projects[ids.pTide]!, active_writer: null };
  host.projects = host.projects.map(p => p.project_id === project.project_id ? project : p);
  host.emit({ type: "project_upserted", project });
  app.ui.selectTask(ids.pTide, null);
  app.ui.saveDraft(ids.pTide, null, { text: "Check the parser", mode: "work", agent: { harness: "claude_code", model: null, effort: null, launchPolicy: "bypass_permissions" } });
  review(); await tick();
  expect(summary()?.textContent).toBe("In tidewater-api · Claude Code · Work, “Skip permissions” · Starts now");
  expect(summary()?.classList.contains("launch-line__risk")).toBe(true);
  expect(summary()?.nextElementSibling).toBe(screen.getByRole("button", { name: "Start task" }));
});

it("follows the choices made further up, without a warning when the agent asks", async () => {
  app.ui.selectTask(ids.pTide, null);
  app.ui.saveDraft(ids.pTide, null, { text: "Check the parser", mode: "work", agent: { harness: "claude_code", model: null, effort: null, launchPolicy: "standard" },
    review: { ...freshReview(), placement: "queue", authorization: "ask_before_start" } });
  review(); await tick();
  expect(summary()?.textContent).toBe("In tidewater-api · Claude Code · Work, “Ask for approval” · Next up, asks you first");
  expect(summary()?.classList.contains("launch-line__risk")).toBe(false);
  expect(screen.getByRole("button", { name: "Add to Next up" })).toBeTruthy();
});
