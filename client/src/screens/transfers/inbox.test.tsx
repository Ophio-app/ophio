// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { DestinationPicker } from "./DestinationPicker";
import { TransferRow } from "./TransferRow";


let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "work" }, depth: 1, active: true }}>{children}</RouteProvider></AppProvider>;
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
  app.ui.selectTask(ids.pTide, null);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});



it("names the real inbox folder in the choice and result", () => {
  const folder = app.session.state.get().host!.inbox_path!;
  render(<DestinationPicker value={{ kind: "inbox" }} onChange={() => {}} />, { wrapper });
  expect(screen.getByRole("radio", { name: `${folder} on studio` })).toBeTruthy();
  render(<TransferRow computer="studio" record={{ transfer_id: "sent", direction: "phone_to_host", name: "note.txt", size: 1, transferred: 1, sha256: null, state: { state: "completed" }, destination: { kind: "inbox" }, destination_folder: folder, host_path: `${folder}/note.txt`, created_at: 0, updated_at: 0 }} online />, { wrapper });
  expect(screen.getByText(`Received in: ${folder} on studio`)).toBeTruthy();
});
