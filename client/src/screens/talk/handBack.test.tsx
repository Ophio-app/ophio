// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { Transcript } from "./Transcript";
import { useHost } from "../../app/context";
import { AttentionList, TaskRow } from "../work/shared";
import { BottomNav } from "../../shell/BottomNav";
import { openAttention } from "../../state/projection";
import { HandBack } from "./HandBack";
import Review from "./Review";
import { annotationStore, blobs } from "../../state/annotations";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 180) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
function view(child: React.ReactNode, route: Route = { screen: "talk" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{child}</RouteProvider></AppProvider>);
}

// The simulated capture draws on a canvas, which these tests have none of;
// the capture itself is the computer's, so what it returns stands in.
const captured = vi.hoisted(() => ({ targets: [] as string[] }));
vi.mock("../annotate/sources", async (actual) => {
  const sources = await actual<typeof import("../annotate/sources")>();
  return { ...sources, captureSource: async (_ctx: unknown, store: Parameters<typeof sources.importSource>[0], id: string, targetId: string) => {
    captured.targets.push(targetId);
    return sources.importSource(store, id, { blob: new Blob(["dp-1"], { type: "image/png" }), mediaType: "image/png", capturedAt: Date.now(), origin: { kind: "phone_import", original_name: "DP-1.png" } }, { artifactId: "capture-dp1" });
  } };
});
vi.mock("../control/useMedia", () => ({ useMedia: (target: { geometry: { revision: number } }) => ({
  picture: { at: Date.now(), revision: target?.geometry.revision, connected: true, error: null }, canvas: { current: null }, retry() {},
}) }));
it("hands this device’s display back after reviewing the current picture", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const task = app.session.state.get().tasks[ids.tDash]!;
  const request = app.session.command({ command: "take_over", geometry_revision: 3, target_id: task.target_id! }, { label: "Take over" });
  await tick(1600); await request;
  view(<HandBack task={app.session.state.get().tasks[task.task_id]!} />);
  expect(screen.getByText(/You’re in control of Screen 1 \(DP-1\)/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Resume agent" }));
  fireEvent.click(screen.getByRole("button", { name: "Resume now" }));
  await tick(900);
  expect(app.session.state.get().control[task.target_id!]?.input_state).toBe("agent");
  expect(app.session.state.get().tasks[task.task_id]?.state).toBe("working");
  // The agent acts only while someone watches its screen, which opens next.
  expect(app.ui.route()).toEqual({ screen: "computer_use", taskId: ids.tDash });
});

function Conversation() {
  const task = useHost(s => s.tasks[ids.tDash]!);
  return <><HandBack task={task} /><Transcript task={task} agent="Claude Code" /></>;
}
it("released input tells the truth; two hand-backs stay in Chat and pair with takeovers once", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  app.ui.selectTask(ids.pTide, ids.tDash);
  view(<Conversation />);
  for (let i = 0; i < 2; i++) {
    const take = app.session.command({ command: "take_over", geometry_revision: 3, target_id: ids.tgDp1 }, { label: "Take over" });
    await tick(1600); await take;
    const lease = app.session.state.get().control[ids.tgDp1]!.lease!;
    const leave = app.session.command({ command: "release_lease", lease_id: lease.lease_id }, { label: "Leave" });
    await tick(500); await leave;
    expect(screen.getByText(/Your control ended · it can’t use Screen 1 \(DP-1\) until you resume it/)).toBeTruthy();
    expect(screen.queryByText(/You’re in control of Screen 1 \(DP-1\)/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Resume agent" }));
    fireEvent.click(screen.getByRole("button", { name: "Resume now" }));
    await tick(900);
    expect(app.ui.route()).toEqual({ screen: "computer_use", taskId: ids.tDash });
    expect(screen.getAllByText(/^Handed back ·/)).toHaveLength(i + 1);
  }
  const lines = [...screen.getByRole("list", { name: "Conversation" }).querySelectorAll(".talk-note--receipt")].map(e => e.textContent);
  expect(lines.filter(s => /You have control|Handed back/.test(s ?? "")).map(s => s?.startsWith("Handed back") ? "back" : "take")).toEqual(["take", "back", "take", "back"]);
});

it("admission without a confirmed effect never renders Handed back", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const take = app.session.command({ command: "take_over", geometry_revision: 3, target_id: ids.tgDp1 }, { label: "Take over" });
  await tick(1600); await take;
  view(<Conversation />); await tick(200);
  vi.spyOn(app.session, "command").mockResolvedValue({ command_id: "admitted-only", status: "accepted", seq: null, result: null, error: null });
  fireEvent.click(screen.getByRole("button", { name: "Resume agent" }));
  fireEvent.click(screen.getByRole("button", { name: "Resume now" }));
  await tick(500);
  expect(screen.queryByText(/^Handed back/)).toBeNull();
});

it("Tasks lists released input once, counts it, opens hand-back, and clears it", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const baseline = openAttention(app.session.state.get()).length;
  const take = app.session.command({ command: "take_over", geometry_revision: 3, target_id: ids.tgDp1 }, { label: "Take over" });
  await tick(1600); await take;
  const lease = app.session.state.get().control[ids.tgDp1]!.lease!;
  const leave = app.session.command({ command: "release_lease", lease_id: lease.lease_id }, { label: "Leave" });
  await tick(500); await leave;
  view(<><AttentionList projectId={ids.pTide} /><BottomNav /></>);
  expect(screen.getAllByText(/Agent paused on Screen 1 \(DP-1\) · Resume agent/)).toHaveLength(1);
  expect(screen.getByRole("button", { name: `Tasks, ${baseline + 1} need attention` })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /Check the staging dashboard.*Agent paused/ }));
  expect(screen.getByRole("dialog", { name: "Resume the agent on Screen 1 (DP-1)" })).toBeTruthy();
  await tick(1100);
  expect(screen.getByRole("button", { name: "Resume now" })).toHaveProperty("disabled", false);
  fireEvent.click(screen.getByRole("button", { name: "Resume now" }));
  await tick(900);
  expect(screen.queryByText(/Agent paused on Screen 1 \(DP-1\) · Resume agent/)).toBeNull();
  expect(screen.getByRole("button", { name: `Tasks, ${baseline} need attention` })).toBeTruthy();
});

it("a blocked Tasks row explains the approval instead of its last activity", () => {
  const task = app.session.state.get().tasks[ids.tFlaky]!;
  const approval = app.session.state.get().approvals[ids.aTest]!;
  window.__sim!.host.live({ kind: "activity_status", task_id: task.task_id, run_id: task.current_run!, text: "Ran retries_on_503 200 times under load" });
  view(<TaskRow task={task} />);
  expect(screen.getByText(`Approval needed · ${approval.title}`)).toBeTruthy();
  expect(screen.queryByText("Ran retries_on_503 200 times under load")).toBeNull();
});

/** DP-1's input is paused with the agent waiting for it back. */
async function leaveDp1() {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const take = app.session.command({ command: "take_over", geometry_revision: 3, target_id: ids.tgDp1 }, { label: "Take over" });
  await tick(1600); await take;
  const lease = app.session.state.get().control[ids.tgDp1]!.lease!;
  const leave = app.session.command({ command: "release_lease", lease_id: lease.lease_id }, { label: "Leave" });
  await tick(500); await leave;
}

it("Mark up captures the waiting screen into the task’s markup, set to go back with the hand-back", async () => {
  vi.stubGlobal("createImageBitmap", async () => ({ width: 1920, height: 1080, close() {} }));
  const stored = new Map<string, Blob>();
  vi.spyOn(blobs, "put").mockImplementation(async (blob) => { const key = `blob-${stored.size}`; stored.set(key, blob); return key; });
  vi.spyOn(blobs, "get").mockImplementation(async (key) => stored.get(key) ?? null);
  await leaveDp1();
  app.ui.selectTask(ids.pTide, ids.tDash);
  view(<Conversation />);
  fireEvent.click(screen.getByRole("button", { name: "Resume agent" }));
  await tick(1100);
  fireEvent.click(screen.getByRole("button", { name: "Mark up" }));
  await act(() => vi.waitFor(() => expect(app.ui.route()).toMatchObject({ screen: "annotate" })));
  const runId = app.session.state.get().tasks[ids.tDash]!.current_run!;
  const route = app.ui.route();
  expect(route).toMatchObject({ screen: "annotate", intent: "steer", handBack: { targetId: ids.tgDp1, runId } });
  const id = route.screen === "annotate" ? route.annotationId : "";
  const draft = annotationStore(app.profile.profileId).get()[id]!;
  expect(draft.sources).toHaveLength(1);
  expect(captured.targets).toEqual([ids.tgDp1]);
  expect(draft.sources[0]!.artifactId).toBe("capture-dp1");
  expect(app.ui.draft(ids.pTide, ids.tDash)?.annotationId).toBe(id);
  // Nothing is handed back until the markup is sent.
  expect(app.session.state.get().control[ids.tgDp1]?.input_state).toBe("paused");
  expect(screen.queryByRole("dialog", { name: "Resume the agent on Screen 1 (DP-1)" })).toBeNull();
  vi.unstubAllGlobals();
});

it("sending from a hand-back’s Review steers the message in, then gives the input back", async () => {
  await leaveDp1();
  const runId = app.session.state.get().tasks[ids.tDash]!.current_run!;
  app.ui.saveDraft(ids.pTide, ids.tDash, { text: "The p95 line is the dashed one" });
  const route: Extract<Route, { screen: "review" }> = { screen: "review", projectId: ids.pTide, taskId: ids.tDash, intent: "steer", handBack: { targetId: ids.tgDp1, runId } };
  app.ui.push(route, "talk");
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: app.ui.get().stacks.talk.indexOf(route), active: true, route }}><Review route={route} /></RouteProvider></AppProvider>);
  expect(screen.getByText(/Then resumes the agent on Screen 1 \(DP-1\)/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Send and resume" }));
  await tick(2500);
  const sent = Object.values(app.session.commands.get()).map(c => c.body.command).filter(c => c === "send_message" || c === "return_to_agent");
  expect(sent).toEqual(["send_message", "return_to_agent"]);
  expect(app.session.state.get().control[ids.tgDp1]?.input_state).toBe("agent");
  expect(app.ui.route()).toEqual({ screen: "computer_use", taskId: ids.tDash });
  const said = (app.session.state.get().timelines[ids.tDash]?.items ?? []).filter(i => i.item.kind === "user_message").map(i => i.item.kind === "user_message" ? i.item.text : "");
  expect(said).toContain("The p95 line is the dashed one");
});
