// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { RouteProvider } from "../../shell/Screen";
import { stopStore } from "../control/stops";
import Talk from "./Talk";

let app: App;
const tick = (ms = 1500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  app.ui.selectProject(ids.pTide);
  await tick(100);
  const route = { screen: "talk" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /></RouteProvider></AppProvider>);
  await tick(100);
});
afterEach(() => { cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const conversation = () => window.__sim!.host.tasks.find((t) => t.conversation)!;
const composer = () => screen.getAllByRole("textbox").at(-1)!;

/** New conversation, its first message, and its reply. */
async function begin(text = "How does the rate limiter decide when to back off?") {
  fireEvent.click(screen.getByRole("button", { name: "New conversation" }));
  await tick(100);
  fireEvent.change(composer(), { target: { value: text } });
  fireEvent.click(screen.getByRole("button", { name: "Start conversation" }));
  await tick(4000);
  return conversation();
}

it("starts a conversation in Ask from Chat, and its first reply is in the chat (D-065)", async () => {
  // New conversation first, with no description, then the conversations.
  expect(screen.getAllByRole("button").find((b) => b.textContent?.includes("conversation"))?.textContent).toBe("New conversation");
  expect(screen.getByRole("heading", { name: "Conversations" })).toBeTruthy();
  expect(screen.getByText("No conversations yet.")).toBeTruthy();
  const started = await begin();
  expect(started).toMatchObject({ mode: "ask", conversation: true, project_id: ids.pTide });
  expect(app.ui.get()).toMatchObject({ taskList: false, taskByProject: { [ids.pTide]: started.task_id } });
  expect(screen.getByRole("button", { name: /^Model for your next message/ }).textContent).toMatch(/Claude Code/);
  expect(screen.getByRole("button", { name: /Turn into a task/ })).toBeTruthy();
  // Between messages it hasn't ended: no "Finished", in the header or the chat.
  expect(document.querySelector(".chat-head__state")).toBeNull();
  expect(screen.queryByText(/^Finished/)).toBeNull();
  expect(screen.getByText(/^Replied · /)).toBeTruthy();
  // The count on the way back is Work's tasks, which a conversation doesn't hold.
  expect(screen.getByRole("button", { name: /^Conversations, \d+ needs? attention in Tasks$/ })).toBeTruthy();
});

it("switches the model for the next message without asking which agent should get it", async () => {
  const started = await begin();
  const before = started.model;
  fireEvent.click(screen.getByRole("button", { name: /^Model for your next message/ }));
  await tick(100);
  const sheet = screen.getByRole("dialog", { name: /Model for your next message/ });
  fireEvent.click(within(sheet).getByRole("button", { name: "Claude Fable (latest)" }));
  fireEvent.click(within(sheet).getByRole("button", { name: "Use from the next message" }));
  await tick(100);
  fireEvent.change(composer(), { target: { value: "And what about 429s?" } });
  expect(screen.queryByText(/uses a different agent or model/)).toBeNull();
  const send = screen.getByRole("button", { name: "Send" });
  expect(send.hasAttribute("disabled")).toBe(false);
  fireEvent.click(send);
  await tick(4000);
  const after = conversation();
  expect(after.model).not.toBe(before);
  expect(after).toMatchObject({ mode: "ask", conversation: true });
  // The first reply needs no name; the one from the new model gives it.
  expect(screen.getAllByText(/^Replied · /).map((d) => d.textContent)).toEqual([expect.stringMatching(/^Replied · \d/), expect.stringMatching(/^Replied · Claude .+ · \d/)]);
});

/** Frees pTide, which tFlaky holds while it waits for approval. */
function freeWriter() {
  const host = window.__sim!.host;
  const project = { ...host.projects.find((p) => p.project_id === ids.pTide)!, active_writer: null };
  act(() => { host.projects = host.projects.map((p) => (p.project_id === ids.pTide ? project : p)); host.emit({ type: "project_upserted", project }); });
}

it("Turn into a task carries the conversation on in Work mode, moving it from Chat to Tasks", async () => {
  const started = await begin();
  freeWriter();
  await tick(100);
  fireEvent.click(screen.getByRole("button", { name: /Turn into a task/ }));
  await tick(100);
  const sheet = screen.getByRole("dialog", { name: /Turn into a task/ });
  fireEvent.click(within(sheet).getByRole("button", { name: "Start the task" }));
  await tick(4000);
  const task = window.__sim!.host.tasks.find((t) => t.task_id === started.task_id)!;
  expect(task).toMatchObject({ mode: "work" });
  expect(task.conversation).toBeFalsy();
});

it("Turn into a task keeps the conversation's access, says all of it, and keeps the access bar", async () => {
  const started = await begin();
  freeWriter();
  fireEvent.click(screen.getByRole("button", { name: "Internet" }));
  await tick(2000);
  expect(conversation().access).toMatchObject({ internet: false, write: [] });
  fireEvent.click(screen.getByRole("button", { name: /Turn into a task/ }));
  await tick(100);
  const sheet = screen.getByRole("dialog", { name: /Turn into a task/ });
  const name = window.__sim!.host.projects.find((p) => p.project_id === ids.pTide)!.name;
  expect(sheet.textContent).toContain(`It keeps this conversation’s access, and may change ${name}.`);
  expect(sheet.textContent).toContain("It can’t reach the internet.");
  fireEvent.click(within(sheet).getByRole("button", { name: "Start the task" }));
  await tick(4000);
  const task = window.__sim!.host.tasks.find((t) => t.task_id === started.task_id)!;
  expect(task.conversation).toBeFalsy();
  expect(task.access).toMatchObject({ internet: false, write: [ids.pTide] });
  expect(screen.getByRole("button", { name: "Internet" }).getAttribute("aria-pressed")).toBe("false");
  expect(screen.getByRole("button", { name: /^Change & run: / }).getAttribute("aria-label")).toBe(`Change & run: ${name}`);
});

it("says which task holds the project when a conversation can't become a task yet, and opens it", async () => {
  await begin();
  fireEvent.click(screen.getByRole("button", { name: /Turn into a task/ }));
  await tick(100);
  const sheet = screen.getByRole("dialog", { name: /Turn into a task/ });
  expect(sheet.textContent).toMatch(/Fix the flaky retry test in the payments client/);
  expect(within(sheet).getByRole("button", { name: "Start the task" }).hasAttribute("disabled")).toBe(true);
  fireEvent.click(within(sheet).getByRole("button", { name: "Open the blocking task" }));
  await tick(100);
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(ids.tFlaky);
});

it("renames a conversation from its More sheet", async () => {
  const started = await begin();
  fireEvent.click(screen.getByRole("button", { name: "More about this conversation" }));
  await tick(100);
  fireEvent.click(within(screen.getByRole("dialog", { name: /This conversation/ })).getByRole("button", { name: /^Rename/ }));
  await tick(100);
  const sheet = screen.getByRole("dialog", { name: /Rename conversation/ });
  fireEvent.change(within(sheet).getByRole("textbox", { name: "Conversation title" }), { target: { value: "Rate limiter backoff" } });
  fireEvent.click(within(sheet).getByRole("button", { name: "Save title" }));
  await tick(1500);
  expect(window.__sim!.host.tasks.find((t) => t.task_id === started.task_id)?.title).toBe("Rate limiter backoff");
  expect(screen.queryByRole("dialog", { name: /Rename conversation/ })).toBeNull();
  expect(screen.getByRole("heading", { level: 1, name: /Rate limiter backoff/ })).toBeTruthy();
});

it("opens a conversation started where a conversation the computer no longer has was open", async () => {
  // As after a reinstall: the phone kept a conversation the computer doesn't have.
  act(() => app.ui.set((s) => ({ ...s, conversing: true, taskList: false, taskByProject: { ...s.taskByProject, [ids.pTide]: "0192f7a1-0000-7000-8000-00000000dead" } })));
  await tick(4000);
  expect(screen.getByText("That conversation isn’t on this computer any more")).toBeTruthy();
  fireEvent.change(composer(), { target: { value: "Where do we log retries?" } });
  fireEvent.click(screen.getByRole("button", { name: "Start conversation" }));
  await tick(4000);
  const started = conversation();
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(started.task_id);
  expect(screen.getByRole("heading", { level: 1, name: /Where do we log retries/ })).toBeTruthy();
});

it("shows Sum up so far as what was asked, and opens to the words sent", async () => {
  await begin();
  fireEvent.click(screen.getByRole("button", { name: "Message tools" }));
  await tick(100);
  fireEvent.click(within(screen.getByRole("dialog", { name: "Message tools" })).getByRole("button", { name: /^Sum up so far/ }));
  await tick(4000);
  const asked = document.querySelector<HTMLDetailsElement>(".talk-user__asked")!;
  expect(asked.querySelector("summary")?.textContent).toBe("Sum up so far");
  expect(asked.open).toBe(false);
  expect(asked.querySelector("p")?.textContent).toMatch(/^Sum up this conversation so far/);
});

it("an archived conversation is kept to read, and talking on starts a new one", async () => {
  const started = await begin();
  fireEvent.click(screen.getByRole("button", { name: /^Conversations/ }));
  await tick(100);
  fireEvent.click(screen.getByRole("button", { name: `Archive ${started.title}` }));
  await tick(100);
  // At once, with Undo, in place of asking first.
  expect(screen.queryByRole("dialog", { name: /Archive conversation/ })).toBeNull();
  await tick(1500);
  expect(app.session.state.base().tasks[started.task_id]?.state).toBe("archived");
  // As when opened from Work: Back says it goes there.
  act(() => { app.ui.selectTab("work"); app.ui.openTask(ids.pTide, started.task_id); });
  await tick(100);
  expect(screen.getByRole("button", { name: /^Back to Tasks, \d+ needs? attention$/ })).toBeTruthy();
  expect(screen.getByText("This conversation is kept to read. To keep chatting, start a new one.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "New conversation" }));
  await tick(100);
  expect(app.ui.get()).toMatchObject({ conversing: true });
  expect(document.querySelector(".chat-head__title-text")?.textContent).toBe("New conversation");
});
