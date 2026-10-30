// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Inbox from "./Inbox";
import { inboxStore } from "./inbox";

afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); });
it("opens the shared-message draft when Chat was left on Settings", async () => {
  vi.useFakeTimers(); localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  const ui = new UiStore(profile.profileId);
  const app: App = { platform, profile, profiles: [profile], session, ui, switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start();
  await act(async () => { await vi.advanceTimersByTimeAsync(900); });
  ui.push({ screen: "settings" }, "talk");
  ui.push({ screen: "inbox" }, "work");
  ui.saveDraft(ids.pTide, null, { text: "Existing draft" });
  inboxStore(profile.profileId).add({ kind: "text", text: "Shared idea", blobKey: null, name: "Idea", mediaType: "text/plain", size: 11, sourceApp: null, projectId: null });
  const command = vi.spyOn(session, "command");
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route: { screen: "inbox" }, depth: 1, active: true }}><Inbox route={{ screen: "inbox" }} /></RouteProvider></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Use in a message" }));
  fireEvent.click(screen.getByRole("button", { name: /tidewater-api/ }));
  expect(ui.route()).toEqual({ screen: "talk" });
  expect(ui.get().projectId).toBe(ids.pTide);
  expect(ui.draft(ids.pTide, null)?.text).toBe("Existing draft\n\nShared idea");
  expect(command).not.toHaveBeenCalled();
  session.stop(); window.__sim?.host.dispose(); ui.persistNow();
});
