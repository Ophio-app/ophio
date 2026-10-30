// SPDX-License-Identifier: Apache-2.0
// Next up waits for an unconfirmed queue change before offering more. When
// the computer restarts before it can say what happened, the phone can never
// learn it: Next up must not stay locked (Pause included) until the app is
// killed.
import { act, cleanup, render, screen, fireEvent } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import NextUp from "./NextUp";

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
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

it("a queue change lost to a computer restart does not lock Next up for good", async () => {
  const host = window.__sim!.host;
  app.ui.selectTask(ids.pTide, null);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 1, active: true, route: { screen: "next_up" } }}><NextUp route={{ screen: "next_up" }} /></RouteProvider></AppProvider>);
  await tick(600);
  const pause = () => screen.getByRole<HTMLButtonElement>("button", { name: "Pause queue" });
  expect(pause().disabled).toBe(false);

  // The computer's service goes down while the request is on its way.
  vi.spyOn(host, "command").mockImplementationOnce(() => new Promise(() => {}));
  fireEvent.click(pause());
  await tick(100);
  await act(async () => window.__sim!.offline());
  await tick(600);
  // While the outcome can still be learned, Next up waits for it.
  expect(pause().disabled).toBe(true);

  // It comes back as a new boot, which can't say what the old one received.
  (host as { bootEpoch: string }).bootEpoch = "0192f7a1-0000-7000-8000-00000000b002";
  await act(async () => window.__sim!.online());
  while (app.session.link.get().state !== "online") await tick(100);
  await tick(3000);
  expect(Object.values(app.session.commands.get()).find(c => c.body.command === "queue_pause")?.phase).toBe("unknown");

  // Nothing will ever settle it: waiting longer changes nothing.
  await tick(120_000);
  expect(Object.values(app.session.commands.get()).find(c => c.body.command === "queue_pause")?.phase).toBe("unknown");
  // The queue as the computer reports it now is what the person acts on.
  expect(host.queue.paused).toBe(false);
  expect(pause().disabled).toBe(false);
});
