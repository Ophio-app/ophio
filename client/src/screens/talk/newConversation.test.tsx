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
import { Toasts } from "../../design/Toast";
import Talk from "./Talk";

let app: App;
const tick = (ms = 1500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  // The simulator hashes agent files; native digests don't settle under fake time.
  vi.stubGlobal("crypto", { getRandomValues: crypto.getRandomValues.bind(crypto), randomUUID: crypto.randomUUID.bind(crypto) });
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  app.ui.selectProject(ids.pTide);
  await tick(100);
  const route = { screen: "talk" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /></RouteProvider></AppProvider>);
  await tick(100);
  fireEvent.click(screen.getByRole("button", { name: "New conversation" }));
  await tick(100);
});
afterEach(() => { cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const conversation = () => window.__sim!.host.tasks.find((t) => t.conversation);
const created = () => Object.values(app.session.commands.get()).map((c) => c.body).find((b) => b.command === "create_task");

it("says under the project what the agent may reach in a new conversation", () => {
  expect(screen.getByRole("heading", { name: "tidewater-api" })).toBeTruthy();
  // A new conversation reads its own project and may look things up.
  expect(document.querySelector(".talk-new__lede")?.textContent)
    .toBe("The agent can read tidewater-api but can’t change anything. It can look things up on the internet. Turn it into a task when you want something done.");
  expect(screen.getByRole("button", { name: "Read: tidewater-api" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Change & run: Nothing" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Internet" }).getAttribute("aria-pressed")).toBe("true");
});

it("turns the internet off from the middle of the bar, says so in green, and starts the conversation without it", async () => {
  render(<Toasts />);
  fireEvent.click(screen.getByRole("button", { name: "Internet" })); await tick(100);
  expect(screen.getByRole("button", { name: "Internet" }).getAttribute("aria-pressed")).toBe("false");
  expect([...document.querySelectorAll(".toast--good")].at(-1)?.textContent).toContain("Internet off");
  expect(document.querySelector(".talk-new__lede")?.textContent).toContain("It can’t reach the internet.");
  fireEvent.click(screen.getByRole("button", { name: "Start from skill" })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: /Start from a skill/ })).getByRole("button", { name: /^setup-phone/ }));
  await tick(4000);
  expect(created()).toMatchObject({ conversation: true, access: { read: [ids.pTide], write: [], internet: false } });
  expect(conversation()?.access).toMatchObject({ internet: false });
});

it("starts a typed conversation with what the bar holds", async () => {
  fireEvent.click(screen.getByRole("button", { name: "Internet" })); await tick(100);
  fireEvent.change(screen.getAllByRole("textbox").at(-1)!, { target: { value: "Where are rate limits set?" } });
  fireEvent.click(screen.getByRole("button", { name: "Start conversation" }));
  await tick(4000);
  expect(created()).toMatchObject({ conversation: true, prompt: "Where are rate limits set?", access: { read: [ids.pTide], write: [], internet: false } });
  // A started conversation keeps it; the next new one starts afresh.
  expect(conversation()?.access).toMatchObject({ internet: false });
  expect(app.ui.get().drafts).not.toHaveProperty(["access"]);
});

it("lets the agent change another project and, after a warning, the whole computer", async () => {
  fireEvent.click(screen.getByRole("button", { name: "Change & run: Nothing" })); await tick(100);
  let sheet = screen.getByRole("dialog", { name: /What the agent may change and run/ });
  // The search is there to filter, not focused, so no keyboard comes up.
  expect(document.activeElement).not.toBe(within(sheet).getByRole("searchbox", { name: "Search projects" }));
  fireEvent.change(within(sheet).getByRole("searchbox", { name: "Search projects" }), { target: { value: "garden" } });
  expect(within(sheet).queryByRole("checkbox", { name: /Whole computer/ })).toBeNull();
  fireEvent.click(within(sheet).getByRole("checkbox", { name: /^garden-planner/ }));
  fireEvent.click(within(sheet).getByRole("button", { name: "Done" })); await tick(100);
  expect(screen.getByRole("button", { name: "Change & run: garden-planner" })).toBeTruthy();
  // What it may change, it may read.
  expect(screen.getByRole("button", { name: "Read: tidewater-api +1" })).toBeTruthy();
  expect(document.querySelector(".talk-new__lede")?.textContent).toMatch(/^The agent can change garden-planner and run commands there, and read tidewater-api\./);

  fireEvent.click(screen.getByRole("button", { name: "Change & run: garden-planner" })); await tick(100);
  sheet = screen.getByRole("dialog", { name: /What the agent may change and run/ });
  fireEvent.click(within(sheet).getByRole("checkbox", { name: /Whole computer/ })); await tick(100);
  const warning = screen.getByRole("dialog", { name: "Let it change your whole computer?" });
  fireEvent.click(within(warning).getByRole("button", { name: "Let it change everything" })); await tick(100);
  fireEvent.click(within(sheet).getByRole("button", { name: "Done" })); await tick(100);
  expect(screen.getByRole("button", { name: "Change & run: Whole computer" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Read: Whole computer" })).toBeTruthy();
  expect(document.querySelector(".talk-new__lede")?.textContent).toMatch(/^The agent can read and change anything on this computer, and run any command\./);
});

it("shows what it was kept from, and Allow lets it at once", async () => {
  render(<Toasts />);
  fireEvent.click(screen.getByRole("button", { name: "Internet" })); await tick(100);
  fireEvent.click(screen.getByRole("button", { name: "Start from skill" })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: /Start from a skill/ })).getByRole("button", { name: /^setup-phone/ }));
  await tick(4000);
  const id = conversation()!.task_id;
  act(() => { window.__sim!.host.touchTask(id, { access_blocked: { host: "crates.io", local: false, write: false, at: Date.now() } }); });
  await tick(100);
  expect(screen.getByText(/tried to/).textContent).toBe("Blocked: tried to reach crates.io");
  fireEvent.click(screen.getByRole("button", { name: "Allow" })); await tick(1500);
  expect(conversation()?.access).toMatchObject({ internet: true, local_network: false });
  expect(conversation()?.access_blocked ?? null).toBeNull();
  expect(screen.queryByText(/tried to/)).toBeNull();
  expect([...document.querySelectorAll(".toast--good")].at(-1)?.textContent).toContain("Internet on");
});

it("looks like controls: each side opens its list from a chevron, and the internet sits in a ring", () => {
  for (const name of ["Read: tidewater-api", "Change & run: Nothing"])
    expect(screen.getByRole("button", { name }).querySelector(".access__chevron")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Internet" }).classList.contains("access__net")).toBe(true);
});

it("spins the control it changed until the computer says the change is in force, which takes longer while the agent works", async () => {
  render(<Toasts />);
  fireEvent.click(screen.getByRole("button", { name: "Start from skill" })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: /Start from a skill/ })).getByRole("button", { name: /^setup-phone/ }));
  await tick(4000);
  const id = conversation()!.task_id;
  act(() => { window.__sim!.host.touchTask(id, { state: "working" }); });
  await tick(100);
  const net = () => screen.getByRole("button", { name: "Internet" });
  fireEvent.click(net()); await tick(100);
  // Kept at once, and too soon to spin.
  expect(conversation()?.access).toMatchObject({ internet: false });
  expect(net().getAttribute("aria-busy")).toBeNull();
  await tick(400);
  expect(net().getAttribute("aria-busy")).toBe("true");
  expect(net().classList.contains("is-busy")).toBe(true);
  expect((net() as HTMLButtonElement).disabled).toBe(true);
  // The toast waits for it too.
  expect([...document.querySelectorAll(".toast--good")].map((t) => t.textContent).join()).not.toContain("Internet off");
  await tick(1500);
  expect(net().getAttribute("aria-busy")).toBeNull();
  expect(net().getAttribute("aria-pressed")).toBe("false");
  expect([...document.querySelectorAll(".toast--good")].at(-1)?.textContent).toContain("Internet off");

  // A side spins in place of its icon, then shows what it now covers.
  fireEvent.click(screen.getByRole("button", { name: "Change & run: Nothing" })); await tick(100);
  const sheet = screen.getByRole("dialog", { name: /What the agent may change and run/ });
  fireEvent.click(within(sheet).getByRole("checkbox", { name: /^garden-planner/ }));
  fireEvent.click(within(sheet).getByRole("button", { name: "Done" })); await tick(500);
  const write = screen.getByRole("button", { name: "Change & run: garden-planner" });
  expect(write.getAttribute("aria-busy")).toBe("true");
  expect(write.querySelector(".access__label .lucide-loader-circle")).toBeTruthy();
  await tick(1500);
  expect(screen.getByRole("button", { name: "Change & run: garden-planner" }).getAttribute("aria-busy")).toBeNull();
});

it("doesn't spin for a change the computer puts in force at once", async () => {
  fireEvent.click(screen.getByRole("button", { name: "Start from skill" })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: /Start from a skill/ })).getByRole("button", { name: /^setup-phone/ }));
  await tick(4000);
  act(() => { window.__sim!.host.touchTask(conversation()!.task_id, { state: "completed" }); });
  await tick(100);
  const net = () => screen.getByRole("button", { name: "Internet" });
  fireEvent.click(net());
  for (let i = 0; i < 6; i++) { await tick(50); expect(net().getAttribute("aria-busy")).toBeNull(); }
  expect(net().getAttribute("aria-pressed")).toBe("false");
});

it("starts a conversation from an agent found by searching, with the agent's name as its first message", async () => {
  fireEvent.click(screen.getByRole("button", { name: "Start from agent" })); await tick();
  const sheet = screen.getByRole("dialog", { name: /Start from an agent/ });
  // Agents alone: no way to skills or the scratch pad from here.
  expect(within(sheet).queryByRole("radio")).toBeNull();
  const search = within(sheet).getByRole("searchbox", { name: "Search agents" });
  expect(document.activeElement).not.toBe(search);
  fireEvent.change(search, { target: { value: "video" } });
  expect(within(sheet).queryByRole("button", { name: /api-reviewer/ })).toBeNull();
  fireEvent.change(search, { target: { value: "no such agent" } });
  expect(within(sheet).getByText("No agent matches “no such agent”.")).toBeTruthy();
  fireEvent.change(search, { target: { value: "video" } });
  fireEvent.click(within(sheet).getByRole("button", { name: /Video producer/ }));
  await tick(4000);
  expect(created()).toMatchObject({ conversation: true, mode: "ask", prompt: "Start with Video producer.", start: { type: "agent", id: "video-producer" } });
  // What the bar holds goes with it: its own project, the internet, nothing changed.
  expect(created()).toMatchObject({ access: { read: [ids.pTide], write: [], whole_read: false, whole_write: false, internet: true } });
  expect(conversation()).toMatchObject({ project_id: ids.pTide, mode: "ask" });
  expect(app.ui.get().taskByProject[ids.pTide]).toBe(conversation()!.task_id);
});

it("starts a conversation from a skill, the project's own among them", async () => {
  fireEvent.click(screen.getByRole("button", { name: "Start from skill" })); await tick();
  const sheet = screen.getByRole("dialog", { name: /Start from a skill/ });
  expect(within(sheet).getByRole("button", { name: /^migrations/ })).toBeTruthy();
  fireEvent.click(within(sheet).getByRole("button", { name: /^setup-phone/ }));
  await tick(4000);
  expect(created()).toMatchObject({ conversation: true, prompt: "Start with setup-phone.", start: { type: "skill", id: "setup-phone" } });
  expect(conversation()).toBeTruthy();
});

it("keeps a refused start on the new conversation, saying why", async () => {
  fireEvent.click(screen.getByRole("button", { name: "Start from skill" })); await tick();
  // The project's own Codex skill asks for Codex, which can't keep Ask read-only here.
  fireEvent.click(within(screen.getByRole("dialog", { name: /Start from a skill/ })).getByRole("button", { name: /^migrations/ }));
  await tick(4000);
  expect(created()).toMatchObject({ harness: "codex", prompt: "Start with migrations.", start: { id: "migrations" } });
  expect(conversation()).toBeUndefined();
  expect(screen.getByText(/cannot confirm that Ask and Plan will leave files unchanged/)).toBeTruthy();
});

it("lets a host on the person's own network in with that, not just the internet, and the internet control shows it", async () => {
  render(<Toasts />);
  fireEvent.click(screen.getByRole("button", { name: "Start from skill" })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: /Start from a skill/ })).getByRole("button", { name: /^setup-phone/ }));
  await tick(4000);
  const id = conversation()!.task_id;
  expect(conversation()?.access).toMatchObject({ internet: true, local_network: false });
  act(() => { window.__sim!.host.touchTask(id, { access_blocked: { host: "nas.tail1234.ts.net", local: true, write: false, at: Date.now() } }); });
  await tick(100);
  expect(screen.getByText(/tried to/).textContent).toBe("Blocked: tried to reach nas.tail1234.ts.net on your own network");
  fireEvent.click(screen.getByRole("button", { name: "Allow" })); await tick(1500);
  expect(conversation()?.access).toMatchObject({ internet: true, local_network: true });
  expect([...document.querySelectorAll(".toast--good")].at(-1)?.textContent).toContain("Your own network on");
  // Off turns both off; on again is the internet alone.
  fireEvent.click(screen.getByRole("button", { name: "Internet and your own network" })); await tick(1500);
  expect(conversation()?.access).toMatchObject({ internet: false, local_network: false });
  fireEvent.click(screen.getByRole("button", { name: "Internet" })); await tick(1500);
  expect(conversation()?.access).toMatchObject({ internet: true, local_network: false });
});
