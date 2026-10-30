// SPDX-License-Identifier: Apache-2.0
// End to end in Talk: a message whose answer was lost, "Send again" tapped
// while the computer is unreachable, then the computer comes back. The
// computer must end up with the message once, never twice.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Talk from "./Talk";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
const tick = async (ms = 180) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

const said = "Zoom the chart to the last 15 minutes.";

it("Send again while offline, then back online, never delivers the message twice", async () => {
  const host = window.__sim!.host;
  const received = vi.spyOn(host, "command");
  const acted = () => new Set(received.mock.calls
    .filter(([env]) => env.body.command === "send_message" && env.body.text === said)
    .map(([env]) => env.command_id)).size;

  app.ui.selectTask(ids.pTide, ids.tDash);
  app.ui.saveDraft(ids.pTide, ids.tDash, { text: said });
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}><Talk route={{ screen: "talk" }} /></RouteProvider></AppProvider>);
  await tick(900);

  // The computer carries out the message, but the connection drops before
  // its answer or its record reaches the phone, and stays down.
  window.__sim!.dropNextReceipt();
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await tick(50);
  await act(async () => window.__sim!.offline());
  await tick(600);
  expect(acted()).toBe(1);
  expect(screen.getByText("It may have arrived")).toBeTruthy();

  // Still offline, the person taps Send again on the notice.
  fireEvent.click(screen.getByRole("button", { name: "Send again" }));
  await tick(600);

  // The computer comes back. Whatever the notice now offers, following it
  // must not make the computer act on the message a second time.
  await act(async () => window.__sim!.online());
  while (app.session.link.get().state !== "online") await tick(100);
  await tick(3000);
  const retry = screen.queryByRole("button", { name: "Send again" });
  if (retry) {
    fireEvent.click(retry);
    await tick(3000);
  }
  expect(acted()).toBe(1);
  expect(app.ui.draft(ids.pTide, ids.tDash)).toBeNull();
});
