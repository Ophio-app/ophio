// SPDX-License-Identifier: Apache-2.0
//
// All computers' Tasks and To-Do: every connected computer's work in one
// list, and whatever is done to an item goes to that item's own computer.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { Computers } from "../../app/computers";
import { UiStore } from "../../app/ui";
import { prefs } from "../../app/prefs";
import { createSimPlatform } from "../../dev/sim/platform";
import { otherIds } from "../../dev/sim/computers";
import { ids } from "../../dev/sim/scenario";
import { RouteProvider } from "../../shell/Screen";
import { BottomNav } from "../../shell/BottomNav";
import type { Command } from "../../protocol/types";
import { AllWork } from "./AllWork";
import Todo from "../checklist/Todo";
import { OnComputer } from "../../app/onComputer";
import { StopTask } from "../control/StopTask";
import { Notice } from "../../design/Notice";
import { ReportQuestionCard } from "../../reports/Questions";
import { reportRequest } from "../../reports/request";
import { phoneIsOlder } from "./AllWork";
import { createPortal } from "react-dom";
import { useApp } from "../../app/context";
import { RequestChecklist } from "../checklist/RequestChecklist";

let app: App;
let release = () => {};
const advance = async (ms = 0) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
async function start(search = "?three-computers") {
  window.history.replaceState(null, "", `/${search}`);
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
}
/** The commands each computer's session sent, by computer. */
const sent = () => Object.fromEntries(["sim-studio", "sim-spare", "sim-laptop"].map((id) => [id,
  Object.values(app.computers!.peek(id)!.commands.get()).map((c) => c.body as Command)]));
const wrap = (node: React.ReactNode, tab: "work" | "todo" = "work") => <AppProvider app={app}>
  <RouteProvider value={{ tab, route: { screen: tab }, depth: 0, active: true }}>{node}</RouteProvider></AppProvider>;
const card = (title: string) => screen.getByText(title).closest(".work-attention-item") as HTMLElement;

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  prefs.set((p) => ({ ...p, allProjects: true, warningsOff: true }));
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  Element.prototype.scrollIntoView ??= () => {};
});
afterEach(() => {
  cleanup(); release(); window.__sim?.host.dispose();
  window.history.replaceState(null, "", "/");
  vi.clearAllTimers(); vi.useRealTimers();
});

it("lists every connected computer's requests and running tasks, each naming its computer", async () => {
  await start();
  render(wrap(<AllWork />));
  const where = (title: string) => (screen.getByText(title).closest(".row, .work-attention-item") as HTMLElement).querySelector(".work-row-where")?.textContent;
  expect(where("Archive last month's renders")).toBe("spare · render-farm");
  expect(where("Sort the winter notes into chapters")).toBe("spare · field-notes");
  expect(where("Render the winter tiles at 4K")).toBe("spare · render-farm");
  expect(screen.getByText("Render the winter tiles at 4K").closest(".row")!.querySelector(".all-projects__computer")!.tagName).toBe("BDI");
  // A result to look at on the laptop.
  expect(where("Write up the Lisbon trip")).toBe("laptop · travel-journal");
});

it("sends Allow on another computer's approval card to that computer", async () => {
  await start();
  render(wrap(<AllWork />));
  fireEvent.click(within(card("Archive last month's renders")).getByRole("button", { name: "Allow" }));
  await advance(900);
  expect(sent()["sim-spare"]!).toContainEqual(expect.objectContaining({ command: "resolve_approval", approval_id: otherIds.spare.aArchive }));
  expect(sent()["sim-studio"]!.filter((c) => c.command === "resolve_approval")).toEqual([]);
  expect(app.computers!.peek("sim-spare")!.state.get().approvals[otherIds.spare.aArchive]?.state).not.toBe("pending");
  // Studio's own request is still waiting.
  expect(app.session.state.get().approvals[ids.aTest]?.state).toBe("pending");
});

it("sends Decline on another computer's approval card to that computer", async () => {
  await start();
  render(wrap(<AllWork />));
  fireEvent.click(within(card("Archive last month's renders")).getByRole("button", { name: /^Decline/ }));
  await advance(300);
  fireEvent.click(within(card("Archive last month's renders")).getByRole("button", { name: "Decline" }));
  await advance(900);
  expect(sent()["sim-spare"]!).toContainEqual(expect.objectContaining({ command: "resolve_approval", approval_id: otherIds.spare.aArchive }));
  expect(sent()["sim-studio"]!.filter((c) => c.command === "resolve_approval")).toEqual([]);
});

it("sends an answer to another computer's question to that computer", async () => {
  await start();
  render(wrap(<AllWork />));
  fireEvent.click(screen.getByText("Sort the winter notes into chapters"));
  await advance(300);
  const sheet = screen.getByRole("dialog", { name: "Review request" });
  fireEvent.click(within(sheet).getByText("By date"));
  await advance(100);
  const send = within(sheet).getAllByRole("button").find((b) => /^(Send|Answer)/.test(b.textContent ?? "") && !(b as HTMLButtonElement).disabled);
  fireEvent.click(send!);
  await advance(900);
  expect(sent()["sim-spare"]!).toContainEqual(expect.objectContaining({ command: "resolve_approval", approval_id: otherIds.spare.aChapters }));
  expect(sent()["sim-studio"]!.filter((c) => c.command === "resolve_approval")).toEqual([]);
});

it("opens another computer's result on the tab it was opened from, above All, so Back returns to All", async () => {
  await start();
  app.ui.selectTab("work");
  render(wrap(<AllWork />));
  fireEvent.click(screen.getByText("Write up the Lisbon trip"));
  await advance(0);
  expect(vi.mocked(app.visit!).mock.calls.map((c) => c[0])).toEqual(["sim-laptop"]);
  const laptopUi = app.uiOf!("sim-laptop");
  laptopUi.selectTab("talk");
  vi.mocked(app.visit!).mock.calls[0]![1](laptopUi);
  expect(laptopUi.get().tab).toBe("work");
  expect(laptopUi.get().stacks.work).toEqual([{ screen: "work" }, expect.objectContaining({ screen: "receipt", taskId: otherIds.laptop.tLisbon })]);
});

it("stops another computer's task there and moves nothing, even with its chat in view", async () => {
  await start();
  const spareUi = app.uiOf!("sim-spare");
  const task = app.computers!.peek("sim-spare")!.state.get().tasks[otherIds.spare.tRender]!;
  // The spare's Talk shows the task, as when the person left it there.
  spareUi.openTask(task.project_id, task.task_id);
  const entry = { profile: app.profiles[1]!, session: app.computers!.peek("sim-spare"), current: false };
  render(wrap(<OnComputer entry={entry}><StopTask task={task} /></OnComputer>));
  fireEvent.click(screen.getByRole("button", { name: /^Stop · Render the winter tiles/ }));
  await advance(900);
  expect(app.visit).not.toHaveBeenCalled();
  expect(sent()["sim-spare"]!).toContainEqual(expect.objectContaining({ command: "stop_task", task_id: task.task_id }));
  expect(sent()["sim-studio"]!.filter((c) => c.command === "stop_task")).toEqual([]);
});

it("counts every computer as on screen only while All's own list is, so others alert once it isn't", async () => {
  await start();
  const route = (active: boolean) => <AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "work" }, depth: 0, active }}><AllWork /></RouteProvider></AppProvider>;
  const { rerender, unmount } = render(route(true));
  expect(app.computers!.showingAll.get()).toBe(true);
  // A chat opened over it, or another tab: the list is no longer on screen.
  rerender(route(false));
  expect(app.computers!.showingAll.get()).toBe(false);
  rerender(route(true));
  expect(app.computers!.showingAll.get()).toBe(true);
  // Gone, as while pairing another computer.
  unmount();
  expect(app.computers!.showingAll.get()).toBe(false);
});

it("counts on the tab bar exactly what All's Tasks lists, leaving out what it doesn't", async () => {
  await start();
  app.ui.selectTab("work");
  render(wrap(<><AllWork /><BottomNav /></>));
  await advance(1500);
  const badge = () => Number(/(\d+) needs? attention/.exec(screen.getByRole("button", { name: /^Tasks/ }).getAttribute("aria-label") ?? "")?.[1] ?? 0);
  const needsYou = () => [...document.querySelectorAll(".work-all .section")].find((sec) => sec.querySelector(".section__title")?.textContent === "Needs you")!;
  const listed = () => needsYou().querySelectorAll(".work-attention-item, .work-attention-group > .row").length;
  expect(badge()).toBeGreaterThan(0);
  expect(badge()).toBe(listed());
  // A request with no task, which All can't list under a project, isn't counted.
  const original = app.session.state.get().attention[ids.atReady]!;
  act(() => app.session.state.set((st) => ({ ...st, attention: { ...st.attention, loose: { ...original, attention_id: "loose", task_id: null } } })));
  await advance(100);
  expect(badge()).toBe(listed());
});

it("says what to do about a computer that no longer accepts this phone, rather than to wait", async () => {
  await start();
  act(() => window.__sim!.revoke());
  await advance(900);
  render(wrap(<AllWork />));
  expect(document.querySelector(".notice")?.textContent).toContain("studio no longer accepts this phone. Pair it again from the computer menu");
  expect(document.querySelector(".notice")?.textContent).not.toContain("once it’s back");
});

it("opens another computer's task by switching to that computer first, then opening it there", async () => {
  await start();
  render(wrap(<AllWork />));
  fireEvent.click(screen.getByText("Render the winter tiles at 4K"));
  await advance(0);
  expect(app.visit).toHaveBeenCalledWith("sim-spare", expect.any(Function));
  // Nothing moved on the computer on screen, nor on the spare before the switch.
  expect(app.ui.get().stacks.talk).toHaveLength(1);
  const spareUi = app.uiOf!("sim-spare");
  expect(spareUi.get().taskByProject[otherIds.spare.tRender]).toBeUndefined();
  vi.mocked(app.visit!).mock.calls[0]![1](spareUi);
  expect(Object.values(spareUi.get().taskByProject)).toContain(otherIds.spare.tRender);
});

it("ticks another computer's to-do on that computer", async () => {
  await start();
  app.ui.selectTab("todo");
  render(wrap(<Todo route={{ screen: "todo" }} />, "todo"));
  await advance(1500);
  const item = screen.getByText("Sign in to the render service").closest("li") as HTMLElement;
  expect(item.textContent).toContain("spare · render-farm");
  fireEvent.click(within(item).getAllByRole("checkbox")[0]!);
  await advance(900);
  expect(sent()["sim-spare"]!).toContainEqual(expect.objectContaining({ command: "checklist_tick" }));
  expect(sent()["sim-studio"]!.filter((c) => c.command === "checklist_tick")).toEqual([]);
});

it("leaves out a computer that is offline and says so, as the header's word does", async () => {
  await start("?three-computers&computer-offline=spare");
  render(wrap(<AllWork />));
  expect(document.querySelector(".notice")?.textContent).toMatch(/^1 offline/);
  expect(screen.getByText(/isn’t connected, so its tasks and requests aren’t shown here/)).toBeTruthy();
  expect(screen.queryByText("Archive last month's renders")).toBeNull();
  expect(screen.getByText("Write up the Lisbon trip")).toBeTruthy();
});

it("adds every connected computer's counts to the tab bar while All is chosen", async () => {
  await start();
  const { rerender } = render(wrap(<BottomNav />));
  await advance(1500);
  const tasks = () => screen.getByRole("button", { name: /^Tasks/ }).getAttribute("aria-label");
  const studioOnly = Object.values(app.session.state.get().attention).filter((a) => a.resolved_at === null).length;
  expect(tasks()).toMatch(/need/);
  const all = Number(/(\d+)/.exec(tasks()!)![1]);
  app = { ...app, all: false };
  rerender(wrap(<BottomNav />));
  await advance(100);
  const one = Number(/(\d+)/.exec(tasks()!)![1]);
  expect(all).toBeGreaterThan(one);
  expect(one).toBeLessThanOrEqual(studioOnly);
});


const spareEntry = () => ({ profile: app.profiles[1]!, session: app.computers!.peek("sim-spare"), current: false });

it("counts every computer as on screen while All's To-Do is, and not once it isn't", async () => {
  await start();
  app.ui.selectTab("todo");
  const { unmount } = render(wrap(<Todo route={{ screen: "todo" }} />, "todo"));
  await advance(300);
  expect(app.computers!.showingAll.get()).toBe(true);
  unmount();
  expect(app.computers!.showingAll.get()).toBe(false);
});

it("never switches computers by itself: a to-do focus the spare kept is cleared there, in place", async () => {
  await start();
  const spareUi = app.uiOf!("sim-spare");
  const farm = Object.values(app.computers!.peek("sim-spare")!.state.get().projects).find((p) => p.name === "render-farm")!;
  // The spare's saved state still focuses its sign-in to-do.
  spareUi.openChecklist(farm.project_id, "render-sign-in");
  expect(spareUi.get().todoItemId).toBe("render-sign-in");
  app.ui.selectTab("todo");
  render(wrap(<Todo route={{ screen: "todo" }} />, "todo"));
  await advance(1500);
  await advance(1500);
  expect(screen.getByText("Sign in to the render service")).toBeTruthy();
  expect(app.visit).not.toHaveBeenCalled();
  expect(spareUi.get().todoItemId).toBeNull();
});

it("files a note's Report from another computer's part through that computer", async () => {
  await start();
  reportRequest.set(null);
  render(wrap(<OnComputer entry={spareEntry()}><Notice tone="unknown" title="Next up · render-farm">The queue couldn’t be read.</Notice></OnComputer>));
  fireEvent.click(screen.getByRole("button", { name: "Report" }));
  expect(reportRequest.get()?.context.profileId).toBe("sim-spare");
  reportRequest.set(null);
});

it("starts a report follow-up from another computer's question through that computer", async () => {
  await start();
  reportRequest.set(null);
  const question = { question_id: "q1", template: { template: "may_we_see" as const, part: "names" as const }, asked_at: 1 };
  render(wrap(<OnComputer entry={spareEntry()}><ReportQuestionCard reportId="r1" question={question} /></OnComputer>));
  fireEvent.click(screen.getAllByRole("button")[0]!);
  expect(reportRequest.get()?.origin).toEqual({ type: "follow_up", report_id: "r1" });
  expect(reportRequest.get()?.context.profileId).toBe("sim-spare");
  reportRequest.set(null);
});

it("tells which side to update when versions don't match", async () => {
  expect(phoneIsOlder("The app supports protocol 1.0; the computer supports 2.0. Update the app on this phone from its original source.")).toBe(true);
  expect(phoneIsOlder("The app supports protocol 2.0; the computer supports 1.0. Update Øphio on the computer from its original source.")).toBe(false);
  await start("?three-computers&incompatible");
  render(wrap(<AllWork />));
  await advance(900);
  expect(document.querySelector(".notice")?.textContent).toContain("studio needs a newer app on this phone. Update the app on this phone");
});

it("switches to another computer after an awaited answer, and opens the new task there", async () => {
  await start();
  const farm = Object.values(app.computers!.peek("sim-spare")!.state.get().projects).find((p) => p.name === "render-farm")!;
  render(wrap(<OnComputer entry={spareEntry()}><RequestChecklist project={farm} /></OnComputer>));
  // Asking for a checklist reads the spare's catalog first, then opens the task.
  fireEvent.click(screen.getByRole("button", { name: "Request checklist" }));
  for (let i = 0; i < 10 && !vi.mocked(app.visit!).mock.calls.length; i++) await advance(500);
  await advance(0);
  expect(vi.mocked(app.visit!).mock.calls.map((c) => c[0])).toEqual(["sim-spare"]);
  const spareUi = app.uiOf!("sim-spare");
  vi.mocked(app.visit!).mock.calls[0]![1](spareUi);
  expect(spareUi.get().tab).toBe("talk");
  expect(spareUi.get().taskByProject[farm.project_id]).toBeNull();
  // Its draft asks for the checklist, kept with the spare.
  expect(spareUi.draft(farm.project_id, null)?.text).toBeTruthy();
  // Nothing moved on the computer on screen.
  expect(app.ui.get().taskByProject[farm.project_id]).toBeUndefined();
});

/** A sheet drawn outside the list, as the sheets made with createPortal are. */
function PortalSheet() {
  const { ui } = useApp();
  return createPortal(<button type="button" onClick={() => ui.push({ screen: "next_up" }, "work")}>Open Next up from the sheet</button>, document.body);
}

it("switches to another computer for a move made in a sheet drawn outside its part of the list", async () => {
  await start();
  render(wrap(<OnComputer entry={spareEntry()}><PortalSheet /></OnComputer>));
  fireEvent.click(screen.getByRole("button", { name: "Open Next up from the sheet" }));
  await advance(0);
  expect(vi.mocked(app.visit!).mock.calls.map((c) => c[0])).toEqual(["sim-spare"]);
  const spareUi = app.uiOf!("sim-spare");
  vi.mocked(app.visit!).mock.calls[0]![1](spareUi);
  expect(spareUi.route()).toEqual({ screen: "next_up" });
});
