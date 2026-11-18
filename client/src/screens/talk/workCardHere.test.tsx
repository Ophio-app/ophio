// SPDX-License-Identifier: Apache-2.0
//
// What a suggestion's card offers besides Start: Do it here, only where the
// chat itself can do it and never leaving it wider than before, and More
// options, which opens Review with what the card said the task may reach.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { ConversationAccess, NewWork, Run, Task } from "../../protocol/types";
import { Toasts } from "../../design/Toast";
import { RouteProvider } from "../../shell/Screen";
import Review from "./Review";
import Talk from "./Talk";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  window.history.replaceState(null, "", "/?cards=1");
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  window.history.replaceState(null, "", "/");
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
});
const tick = async (ms = 300) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const host = () => window.__sim!.host;
const chat = (): Task => app.session.state.get().tasks[ids.tOneCard]!;
const reads: ConversationAccess = { read: [ids.pTide], write: [], whole_read: false, whole_write: false, internet: false, local_network: false };
const view = (route: Route = { screen: "talk" }) => render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: route.screen === "talk" ? 0 : 1, active: true, route }}>
  {route.screen === "review" ? <Review route={route} /> : <Talk route={{ screen: "talk" }} />}</RouteProvider><Toasts /></AppProvider>);
/** The one-suggestion chat, with what its chat may reach and what its turn suggested. */
async function open(access: ConversationAccess | null, work?: NewWork) {
  if (access) host().emit({ type: "task_upserted", task: { ...chat(), access } });
  if (work) {
    const run = app.session.state.get().runs[ids.rOneCard]!;
    host().emit({ type: "run_upserted", run: { ...run, outcome: { ...run.outcome!, report: { ...run.outcome!.report!, new_work: [work] } } } });
  }
  app.ui.selectProject(ids.pTide);
  app.ui.selectTask(ids.pTide, ids.tOneCard);
  view(); await tick(600);
}
const picker = () => {
  fireEvent.click(screen.getByRole("button", { name: /^Agent for this task:/ }));
  return within(screen.getByRole("dialog", { name: "Agent for this task" }));
};

it("does the work here with the chat's revision after its access changed, so the computer takes it", async () => {
  await open(reads);
  const was = chat().revision;
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(picker().getByRole("button", { name: /Do it here instead/ }));
  await tick(3000);
  const sent = command.mock.calls.map((c) => [c[0].command, c[1]?.expectedRevision]);
  expect(sent.map((s) => s[0])).toEqual(["set_conversation_access", "resume_task"]);
  // Widening is a change to the chat: the message goes to the chat as it is now.
  expect(sent[1]![1]).toBe(was + 1);
  expect(chat().access?.write).toEqual([ids.pTide]);
  expect(screen.queryByText(/The task changed/)).toBeNull();
});

it("sets the chat back when nothing started here after widening it, and says so", async () => {
  await open(reads);
  const real = app.session.command.bind(app.session);
  vi.spyOn(app.session, "command").mockImplementation((body, opts) => body.command === "resume_task"
    ? Promise.reject(Object.assign(new Error("The agent can’t take a message now."), { code: "unavailable" }))
    : real(body, opts));
  fireEvent.click(picker().getByRole("button", { name: /Do it here instead/ }));
  await tick(4000);
  expect(chat().access?.write).toEqual([]);
  expect(screen.getByText(/Nothing started here, and the chat can no longer change tidewater-api/)).toBeTruthy();
});

it("offers Do it here on an earlier turn's card once a later turn has ended", async () => {
  // A chat's last turn stays its current run after it ends.
  const later: Run = { ...app.session.state.get().runs[ids.rOneCard]!, run_id: "0192f7a1-0000-7000-8000-0000000003ff", outcome: null };
  host().emit({ type: "run_upserted", run: later });
  host().emit({ type: "task_upserted", task: { ...chat(), current_run: later.run_id } });
  await open(reads);
  expect(picker().getByRole("button", { name: /Do it here instead/ })).toBeTruthy();
});

it("doesn't offer Do it here for work in another project, or a question the chat can't reach", async () => {
  await open(reads, { title: "Tidy the beds", brief: "Tidy the planting beds list.", mode: "work",
    needs: { read: [], changes: "garden-planner", internet: false, local_network: false, whole_computer: false, desktop: false } });
  expect(picker().queryByRole("button", { name: /Do it here instead/ })).toBeNull();
  cleanup();
  await open(reads, { title: "Look up the limits", brief: "Find the upload size limits in the provider's docs.", mode: "ask",
    needs: { read: [], changes: null, internet: true, local_network: false, whole_computer: false, desktop: false } });
  expect(picker().queryByRole("button", { name: /Do it here instead/ })).toBeNull();
});

it("opens More options in Review with what the card said the question may reach, and starts it with that", async () => {
  await open(reads, { title: "Look up the limits", brief: "Find the upload size limits in the provider's docs.", mode: "ask",
    needs: { read: [], changes: null, internet: true, local_network: false, whole_computer: false, desktop: false } });
  expect(screen.getByText(/this project only, uses the internet/)).toBeTruthy();
  fireEvent.click(picker().getByRole("button", { name: /More options/ }));
  await tick();
  expect(app.ui.draft(ids.pTide, null)?.access).toMatchObject({ read: [ids.pTide], write: [], internet: true });
  cleanup();
  view(app.ui.route()); await tick(600);
  document.querySelectorAll<HTMLButtonElement>('.fold__head[aria-expanded="false"]').forEach((b) => fireEvent.click(b));
  // Shown, and changeable, where the task starts.
  expect(screen.getByRole("button", { name: /What the agent may reach\. Read: tidewater-api\. Change: Nothing\. Internet on\./ })).toBeTruthy();
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick(1500);
  const made = command.mock.calls.map((c) => c[0]).find((b) => b.command === "create_task");
  expect(made).toMatchObject({ mode: "ask", access: { read: [ids.pTide], write: [], internet: true } });
});

it("lets the person change in Review what a suggested question may reach, and starts it with their choice", async () => {
  await open(reads, { title: "Look up the limits", brief: "Find the upload size limits in the provider's docs.", mode: "ask",
    needs: { read: [], changes: null, internet: true, local_network: false, whole_computer: false, desktop: false } });
  fireEvent.click(picker().getByRole("button", { name: /More options/ }));
  await tick();
  cleanup();
  view(app.ui.route()); await tick(600);
  document.querySelectorAll<HTMLButtonElement>('.fold__head[aria-expanded="false"]').forEach((b) => fireEvent.click(b));
  fireEvent.click(screen.getByRole("button", { name: /^What the agent may reach\./ }));
  // Nothing on the computer changes yet: the choice goes with the task.
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(screen.getByRole("button", { name: "Internet", pressed: true })); await tick();
  expect(command.mock.calls.map((c) => c[0].command)).toEqual([]);
  expect(screen.getByRole("button", { name: /^What the agent may reach\..*Internet off\./ })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick(1500);
  const made = command.mock.calls.map((c) => c[0]).find((b) => b.command === "create_task");
  expect(made).toMatchObject({ mode: "ask", access: { read: [ids.pTide], write: [], internet: false } });
});
