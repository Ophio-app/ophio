// SPDX-License-Identifier: Apache-2.0
//
// A project's chats start with what the person last let a chat there reach:
// held on the computer, set only by the person's own choices, shown only
// once the computer has it, marked when it is more than reading, and set back
// to read-only from one place.
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
  vi.stubGlobal("crypto", { getRandomValues: crypto.getRandomValues.bind(crypto), randomUUID: crypto.randomUUID.bind(crypto) });
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  app.ui.selectProject(ids.pTide);
  await tick(100);
  const route = { screen: "talk" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /></RouteProvider></AppProvider>);
  render(<Toasts />);
  await tick(100);
});
afterEach(() => { cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const tide = () => window.__sim!.host.projects.find((p) => p.project_id === ids.pTide)!;
const sent = (command: string) => Object.values(app.session.commands.get()).map((c) => c.body).filter((b) => b.command === command);
const line = () => document.querySelector(".access__line");
function control(name: string | RegExp): HTMLElement {
  if (line()?.getAttribute("aria-expanded") === "false") fireEvent.click(line()!);
  return screen.getByRole("button", { name });
}

async function newChat() {
  fireEvent.click(screen.getByRole("button", { name: /^New conversation/ }));
  await tick(100);
}

/** Lets a new chat change garden-planner, from the change sheet. */
async function letChangeGarden() {
  fireEvent.click(control("Change & run: Nothing")); await tick(100);
  const sheet = screen.getByRole("dialog", { name: /What the agent may change and run/ });
  fireEvent.click(within(sheet).getByRole("checkbox", { name: /^garden-planner/ }));
  fireEvent.click(within(sheet).getByRole("button", { name: "Done" })); await tick(100);
}

async function start(text: string) {
  fireEvent.change(screen.getAllByRole("textbox").at(-1)!, { target: { value: text } });
  fireEvent.click(screen.getByRole("button", { name: "Start conversation" }));
  await tick(4000);
}

it("starts a project that was never widened read-only, with nothing to set back", async () => {
  await newChat();
  expect(tide().chat_access).toBeUndefined();
  expect(line()?.textContent).toContain("Read: tidewater-api · Change: Nothing");
  expect(document.querySelector(".access__wider")).toBeNull();
  fireEvent.click(line()!);
  expect(screen.queryByRole("button", { name: "Set back to read-only" })).toBeNull();
});

it("remembers what the person let a new chat reach once the chat is made, not while they choose", async () => {
  await newChat();
  await letChangeGarden();
  // Chosen, not yet sent: nothing is kept on the computer.
  expect(sent("set_chat_access_default")).toHaveLength(0);
  // More than read is marked on the line.
  expect(document.querySelector(".access__wider")?.textContent).toBe("More than read-only");
  await start("Rename the planner's seed list");
  expect(sent("set_chat_access_default")).toHaveLength(1);
  expect(tide().chat_access).toMatchObject({ write: [ids.pGarden], read: expect.arrayContaining([ids.pTide, ids.pGarden]) });
  // The next new chat starts with it, and says so.
  fireEvent.click(screen.getByRole("button", { name: /^Conversations/ })); await tick(100);
  await newChat();
  expect(line()?.textContent).toContain("Change: garden-planner");
  expect(document.querySelector(".access__wider")).toBeTruthy();
});

it("remembers a change in a chat only after the computer has it in force", async () => {
  await newChat();
  await start("Where are rate limits set?");
  expect(sent("set_chat_access_default")).toHaveLength(0);
  fireEvent.click(control("Internet"));
  // Sent to the chat first; the memory follows once it's confirmed.
  expect(sent("set_chat_access_default")).toHaveLength(0);
  await tick(2000);
  expect(sent("set_chat_access_default")).toHaveLength(1);
  expect(tide().chat_access).toMatchObject({ internet: false });
});

it("sets new chats back to read-only from the one place that says what they start with", async () => {
  await newChat();
  await letChangeGarden();
  await start("Tidy the seed list");
  fireEvent.click(screen.getByRole("button", { name: /^Conversations/ })); await tick(100);
  await newChat();
  fireEvent.click(line()!);
  expect(screen.getByText(/New chats in tidewater-api start with what you last let a chat here reach\./)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Set back to read-only" }));
  await tick(2000);
  expect(tide().chat_access).toBeUndefined();
  expect([...document.querySelectorAll(".toast--good, .toast")].map((t) => t.textContent).join()).toContain("New chats here start read-only");
  expect(line()?.textContent).toContain("Change: Nothing");
  expect(document.querySelector(".access__wider")).toBeNull();
  expect(screen.queryByRole("button", { name: "Set back to read-only" })).toBeNull();
});

it("starts a chat without a remembered project the computer no longer lists, and remembers it without it", async () => {
  const gone = "0192f7a1-0000-7000-8000-00000000dead";
  const host = window.__sim!.host;
  const remembered = { read: [ids.pTide, ids.pGarden, gone], write: [gone], whole_read: false, whole_write: false, internet: true, local_network: false };
  host.emit({ type: "project_upserted", project: { ...tide(), chat_access: remembered } });
  await tick(100);
  await newChat();
  expect(line()?.textContent).toContain("Read: tidewater-api +1 · Change: Nothing");
  await start("What does the upload path do?");
  const made = sent("create_task").at(-1)!;
  expect(made).toMatchObject({ access: { read: [ids.pTide, ids.pGarden], write: [] } });
  // The chat is wider than reading its own project, so it is remembered: cleaned.
  expect(sent("set_chat_access_default").at(-1)).toMatchObject({ access: { read: [ids.pTide, ids.pGarden], write: [] } });
  expect(tide().chat_access).toMatchObject({ read: [ids.pTide, ids.pGarden], write: [] });
  expect(screen.queryByText(/isn’t on this computer|no longer/)).toBeNull();
});
