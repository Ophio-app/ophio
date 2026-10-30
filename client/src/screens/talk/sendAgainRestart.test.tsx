// SPDX-License-Identifier: Apache-2.0
// "Send again" refuses to repeat a send once the computer has restarted
// (send.ts: resend). When that is found only after the outbox write, the
// composer must come back with the reason, not stay "Sending" for good.
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

it("Send again refused after a computer restart leaves the composer usable and says why", async () => {
  const host = window.__sim!.host;
  app.ui.selectTask(ids.pTide, ids.tDash);
  app.ui.saveDraft(ids.pTide, ids.tDash, { text: said });
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route: { screen: "talk" } }}><Talk route={{ screen: "talk" }} /></RouteProvider></AppProvider>);
  await tick(900);

  // The request is lost on its way and the link drops.
  vi.spyOn(host, "command").mockImplementationOnce(() => new Promise(() => {}));
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await tick(200);
  await act(async () => window.__sim!.offline());
  await tick(600);
  expect(screen.getByText("It may have arrived")).toBeTruthy();

  // Send again: its outbox write is slow, and meanwhile the computer comes
  // back as a new boot, so the repeat can no longer be deduplicated.
  let finish!: () => void;
  vi.spyOn(app.platform.outbox!, "put").mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
  fireEvent.click(screen.getByRole("button", { name: "Send again" }));
  await tick(50);
  (host as { bootEpoch: string }).bootEpoch = "0192f7a1-0000-7000-8000-00000000b002";
  await act(async () => window.__sim!.online());
  while (app.session.link.get().state !== "online") await tick(100);
  await act(async () => { finish(); await vi.advanceTimersByTimeAsync(0); });
  await tick(600);

  // Nothing was repeated; the draft says why and the composer is usable.
  expect(app.ui.draft(ids.pTide, ids.tDash)?.attempt).toMatchObject({ phase: "unknown" });
  expect(screen.getByText("It may have arrived")).toBeTruthy();
  expect(screen.queryByText("Sending to studio")).toBeNull();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Send anyway" }).disabled).toBe(false);
});
