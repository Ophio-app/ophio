// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Review from "./Review";
import { sendAsked, sendOnOpen } from "./sendOnOpen";

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
const tick = async (ms = 500) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
const route = { screen: "review", projectId: ids.pGarden, taskId: null, intent: "new_task" } satisfies Route;
function open(text: string, asked = true) {
  app.ui.saveDraft(ids.pGarden, null, { text, mode: "ask" });
  if (asked) sendOnOpen(ids.pGarden, null);
  app.ui.push(route, "talk");
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}><Review route={route} /></RouteProvider></AppProvider>);
  return vi.spyOn(app.session, "command");
}
const sent = (command: ReturnType<typeof open>) => command.mock.calls.map(([c]) => c.command);

it("sends what a button asked for as Review opens, and lands on the new task", async () => {
  const command = open("Tidy the planting notes");
  await tick(1500);
  expect(sent(command)).toEqual(["create_task"]);
  expect(command.mock.calls[0]![0]).toMatchObject({ prompt: "Tidy the planting notes", mode: "ask" });
  expect(app.ui.draft(ids.pGarden, null)).toBeNull();
  expect(app.ui.get().stacks.talk.at(-1)).not.toBe(route);
  // The ask is taken: the next Review of this draft waits for its button.
  expect(sendAsked(ids.pGarden, null)).toBe(false);
});

it("waits while something stands in the way, and sends once it's cleared", async () => {
  window.__sim!.offline();
  await tick(1500);
  const command = open("Water the tomatoes");
  await tick(1500);
  expect(command).not.toHaveBeenCalled();
  expect(document.querySelector(".review-foot")?.textContent).toMatch(/^Connect to .* to send\./);
  window.__sim!.online();
  await tick(3000);
  expect(sent(command)).toEqual(["create_task"]);
});

it("leaves it to the person once they touch anything first", async () => {
  window.__sim!.offline();
  await tick(1500);
  const command = open("Water the tomatoes");
  await tick(500);
  const field = screen.getByRole("textbox", { name: "What you’re asking" });
  fireEvent.pointerDown(field);
  fireEvent.change(field, { target: { value: "Water the tomatoes and the beans" } });
  window.__sim!.online();
  await tick(3000);
  expect(command).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Start task" }));
  await tick(1500);
  expect(sent(command)).toEqual(["create_task"]);
  expect(command.mock.calls[0]![0]).toMatchObject({ prompt: "Water the tomatoes and the beans" });
});

it("never sends a Review no button asked to", async () => {
  const command = open("Tidy the planting notes", false);
  await tick(1500);
  expect(command).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "Start task" })).toHaveProperty("disabled", false);
});
