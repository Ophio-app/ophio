// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, useHost, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { annotationStore, type SourceDraft } from "../../state/annotations";
import Annotate from "../annotate/Annotate";
import Desktop from "../control/Desktop";
import SafetyStrip from "../control/SafetyStrip";
import { TaskRow } from "../work/shared";
import { HandBack } from "./HandBack";
import Review from "./Review";
import Talk from "./Talk";
import { Transcript } from "./Transcript";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 180) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
vi.mock("../control/useMedia", () => ({ useMedia: (target: { geometry: { revision: number } }) => {
  const picture = { at: Date.now(), revision: target?.geometry.revision, connected: true, error: null };
  return { picture, latest: { current: picture }, canvas: { current: null }, retry() {}, reopen() {} };
} }));

function Surfaces() {
  const task = useHost(s => s.tasks[ids.tDash]!);
  return <>
    <section aria-label="Talk"><HandBack task={task} /><Transcript task={task} agent="Claude Code" /></section>
    <section aria-label="Work"><TaskRow task={task} /></section>
    <section aria-label="Rail"><SafetyStrip /></section>
    <section aria-label="DP-1"><Desktop route={{ screen: "desktop", targetId: ids.tgDp1 }} /></section>
  </>;
}
const surface = (name: string) => within(screen.getByRole("region", { name }));

it("a message after control ended keeps Give input back on Chat, the rail, DP-1 and Tasks", async () => {
  app.ui.selectTask(ids.pTide, ids.tDash);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: { screen: "talk" } }}><Surfaces /></RouteProvider></AppProvider>);
  const take = app.session.command({ command: "take_over", geometry_revision: 3, target_id: ids.tgDp1 }, { label: "Take over" });
  await tick(1600); await take;
  const leave = app.session.command({ command: "release_lease", lease_id: app.session.state.get().control[ids.tgDp1]!.lease!.lease_id }, { label: "Leave" });
  await tick(500); await leave;
  const send = app.session.command({ command: "send_message", task_id: ids.tDash, text: "Also check the legend.", bundle: null, delivery: "next_turn" }, { label: "Send" });
  await tick(600); await send;
  await tick(5000);
  expect(app.session.state.get().tasks[ids.tDash]?.status_line).not.toMatch(/took over/i);

  fireEvent.click(surface("Rail").getByRole("button", { name: /^Safety/ }));
  const waiting = () => {
    expect(surface("Talk").getByText(/Your control ended · it can’t use Screen 1 \(DP-1\) until you resume it/)).toBeTruthy();
    expect(surface("Talk").queryByText("Your control ended · resume it when you’re ready")).toBeNull();
    expect(surface("Work").getByText("Agent paused on Screen 1 (DP-1) · Resume agent")).toBeTruthy();
    expect(surface("Rail").getByText("Screen 1 (DP-1) · Agent paused")).toBeTruthy();
    for (const name of ["Talk", "Rail", "DP-1"]) expect(surface(name).getByRole("button", { name: "Resume agent" })).toBeTruthy();
    expect(surface("DP-1").getByRole("button", { name: "Take over" })).toBeTruthy();
  };
  waiting();
  // A second message while waiting changes nothing either.
  const again = app.session.command({ command: "send_message", task_id: ids.tDash, text: "And the axis.", bundle: null, delivery: "next_turn" }, { label: "Send" });
  await tick(600); await again; await tick(5000);
  waiting();

  fireEvent.click(surface("DP-1").getByRole("button", { name: "Resume agent" }));
  fireEvent.click(screen.getByRole("button", { name: "Resume now" }));
  await tick(900);
  expect(app.session.state.get().control[ids.tgDp1]?.input_state).toBe("agent");
  for (const name of ["Talk", "Rail", "DP-1"]) expect(surface(name).queryByRole("button", { name: "Resume agent" })).toBeNull();
  expect(surface("Work").queryByText(/Agent paused on Screen 1 \(DP-1\)/)).toBeNull();
});

/** Every label the Review's primary action shows from its press until the Review closes. */
type ReviewRoute = Extract<Route, { screen: "review" }>;
async function pressAndWatch(route: ReviewRoute, name: string, during: () => void = () => {}) {
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: app.ui.get().stacks.talk.indexOf(route), active: true, route }}><Review route={route} /></RouteProvider></AppProvider>);
  await tick(600);
  window.__sim!.latency(1500);
  fireEvent.click(screen.getByRole("button", { name }));
  const labels = new Set<string>();
  while (app.ui.get().stacks.talk.includes(route)) {
    await tick(50);
    if (!app.ui.get().stacks.talk.includes(route)) break;
    labels.add(document.querySelector(".review-foot .btn--primary")?.textContent ?? "");
    during();
  }
  return [...labels];
}

it("Start task says Starting… until it has started, though the new task takes the project's writer", async () => {
  const route: ReviewRoute = { screen: "review", projectId: ids.pGarden, taskId: null, intent: "new_task" };
  app.ui.saveDraft(ids.pGarden, null, { text: "Tidy the planting notes", mode: "work" });
  app.ui.push(route, "talk");
  let writer = false;
  const labels = await pressAndWatch(route, "Start task", () => { writer ||= !!app.session.state.get().projects[ids.pGarden]?.active_writer; });
  // The new task held the writer while the Review still waited for the answer.
  expect(writer).toBe(true);
  expect(labels).toEqual(["Starting…"]);
});

it("Start task hands focus to the new task's heading once Chat shows it", async () => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const route: ReviewRoute = { screen: "review", projectId: ids.pGarden, taskId: null, intent: "new_task" };
  app.ui.selectTask(ids.pGarden, null);
  app.ui.saveDraft(ids.pGarden, null, { text: "Tidy the planting notes", mode: "work" });
  app.ui.push(route, "talk");
  render(<AppProvider app={app}>
    <RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}><Talk route={{ screen: "talk" }} /></RouteProvider>
    <RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}><Review route={route} /></RouteProvider>
  </AppProvider>);
  await tick(600);
  const start = screen.getByRole("button", { name: "Start task" });
  start.focus();
  fireEvent.click(start);
  await tick(3000);
  expect(app.ui.get().stacks.talk).toEqual([{ screen: "talk" }]);
  const heading = document.activeElement!;
  expect(heading.matches("h1.chat-head__heading")).toBe(true);
  expect(heading.textContent).toContain("Tidy the planting notes");
});

it("Add to Next up says Adding to Next up… until it is added", async () => {
  const route: ReviewRoute = { screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" };
  app.ui.saveDraft(ids.pTide, null, { text: "Tidy the retry notes", mode: "work" });
  app.ui.push(route, "talk");
  expect(await pressAndWatch(route, "Add to Next up")).toEqual(["Adding to Next up…"]);
});

const said = "Zoom the chart to the last 15 minutes.";
const inConversation = () => screen.queryByRole("list", { name: "Conversation" })?.textContent?.includes(said) ?? false;
async function talkWithDraft() {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  app.ui.selectTask(ids.pTide, ids.tDash);
  app.ui.saveDraft(ids.pTide, ids.tDash, { text: said });
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}><Talk route={{ screen: "talk" }} /></RouteProvider></AppProvider>);
  await tick(900);
}

it("a send whose answer is lost resolves from the computer's record of it, never beside the message", async () => {
  await talkWithDraft();
  window.__sim!.latency(1500);
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await tick(200);
  // The computer has recorded the message; its answer is still on the way.
  expect(inConversation()).toBe(true);
  await act(async () => window.__sim!.offline());
  for (let i = 0; i < 20; i++) {
    expect(screen.queryByText("It may have arrived")).toBeNull();
    await tick(100);
  }
  expect(app.ui.draft(ids.pTide, ids.tDash)?.attempt ?? null).toBeNull();
  expect(screen.getByRole<HTMLTextAreaElement>("textbox").value).toBe("");
});

it("while a send's fate is unknown, Send waits and Check and Send again remain", async () => {
  await talkWithDraft();
  // The request is lost on its way, then the link drops.
  vi.spyOn(window.__sim!.host, "command").mockImplementationOnce(() => new Promise(() => {}));
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await tick(200);
  await act(async () => window.__sim!.offline());
  await tick(600);
  expect(screen.getByText("It may have arrived")).toBeTruthy();
  const box = screen.getByRole<HTMLTextAreaElement>("textbox");
  fireEvent.change(box, { target: { value: `${said} Also the legend.` } });
  // Reconnected, before the computer has answered whether it has the message.
  window.__sim!.latency(1500);
  await act(async () => window.__sim!.online());
  while (app.session.link.get().state !== "online") await tick(100);
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Send" }).disabled).toBe(true);
  expect(screen.getByText("Check whether studio received your last message first, or choose Send again.")).toBeTruthy();
  fireEvent.keyDown(box, { key: "Enter", ctrlKey: true });
  await tick(100);
  expect(app.ui.draft(ids.pTide, ids.tDash)?.attempt?.phase).toBe("unknown");
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Check" }).disabled).toBe(false);
  expect(screen.getByRole("button", { name: "Send again" })).toBeTruthy();
  // The computer never received it, and the edit is still there to send.
  await tick(1500);
  // Nothing is in doubt and the computer is back, so the note no longer warns.
  expect(screen.getByText("Not sent yet")).toBeTruthy();
  expect(document.querySelector(".composer__notice .notice__body")?.textContent).toBe("studio never received it. studio is connected now.");
  expect(inConversation()).toBe(false);
  expect(box.value).toBe(`${said} Also the legend.`);
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Send" }).disabled).toBe(false);
});

it("a withdrawn message put back in the composer says so until the draft changes, and cannot be put back twice", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.task_id === ids.tDash)!;
  const words = "Also check the p99 line.";
  await host.command({ command_id: crypto.randomUUID(), issued_at: Date.now(), expires_at: null, expected_revision: null,
    body: { command: "send_message", task_id: task.task_id, text: words, bundle: null, delivery: "next_turn" } });
  await host.command({ command_id: crypto.randomUUID(), issued_at: Date.now(), expires_at: null, expected_revision: null,
    body: { command: "stop_task", task_id: task.task_id } });
  await tick(1500);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: { screen: "talk" } }}>
    <Transcript task={app.session.state.get().tasks[task.task_id]!} agent="Claude Code" />
  </RouteProvider></AppProvider>);
  await tick();
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Put back in composer" })));
  expect(app.ui.draft(task.project_id, task.task_id)?.text).toBe(words);
  expect(screen.getByText("Back in the composer")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Put back in composer" })).toBeNull();
  // Once the composer no longer holds those words, they can be put back again.
  await act(async () => app.ui.set(s => ({ ...s, drafts: {} })));
  expect(screen.queryByText("Back in the composer")).toBeNull();
  expect(screen.getByRole("button", { name: "Put back in composer" })).toBeTruthy();
});

const shot: SourceDraft = { key: "S1", blobKey: "image", artifactId: "artifact", sha256: "abc", mediaType: "image/png", width: 640, height: 360,
  durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 123, derivedFrom: null, derivation: null };
/** Marks up a screenshot from Talk's composer, then returns how the markup's Review says it will be sent. */
async function markUpFromTalk(taskId: string, chip: string) {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  app.ui.selectTask(ids.pTide, taskId);
  const talk = render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}><Talk route={{ screen: "talk" }} /></RouteProvider></AppProvider>);
  await tick(900);
  const how = document.querySelector(".composer__chip");
  if (!(how instanceof HTMLElement)) throw new Error("The composer has no send chip");
  const says = () => how.textContent?.replace(/^How to send: /, "");
  if (says() !== chip) {
    fireEvent.click(how);
    fireEvent.click(within(screen.getByRole("dialog", { name: "How to send" })).getByText(chip));
  }
  expect(says()).toBe(chip);
  fireEvent.click(screen.getByRole("button", { name: "Attach" }));
  fireEvent.click(within(screen.getByRole("dialog", { name: "Attach" })).getByRole("button", { name: /^Mark up/ }));
  talk.unmount();
  const markup = app.ui.route();
  if (markup.screen !== "annotate") throw new Error(`Expected the markup, not ${markup.screen}`);
  annotationStore(app.profile.profileId).record(markup.annotationId, v => ({ ...v, sources: [shot], selectedSource: shot.key }));
  const annotate = render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: markup }}><Annotate route={markup} /></RouteProvider></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Review and send" }));
  annotate.unmount();
  const review = app.ui.route();
  if (review.screen !== "review") throw new Error(`Expected the Review, not ${review.screen}`);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 2, active: true, route: review }}><Review route={review} /></RouteProvider></AppProvider>);
  await tick(600);
  return review.intent;
}

it("a markup's Review sends it the way the composer's chip said, Steer now on a working task", async () => {
  expect(await markUpFromTalk(ids.tDash, "Steer now")).toBe("steer");
  expect(screen.getByText("Steer now.")).toBeTruthy();
  expect(screen.queryByText("After this turn.")).toBeNull();
});

it("a chip changed before marking up carries through to the Review", async () => {
  // Another task's hold on the project would leave no chip to change.
  const host = window.__sim!.host;
  const project = { ...host.projects.find(p => p.project_id === ids.pTide)!, active_writer: null };
  host.projects = host.projects.map(p => p.project_id === project.project_id ? project : p);
  host.emit({ type: "project_upserted", project });
  expect(await markUpFromTalk(ids.tHeaders, "Start a new task")).toBe("new_task");
  expect(screen.queryByText("Continue.")).toBeNull();
  expect(screen.getByText("This becomes a separate task. “Add rate-limit headers to /v1/orders” isn’t changed.")).toBeTruthy();
});
