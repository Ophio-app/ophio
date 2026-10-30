// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";

import ProjectSheet from "./ProjectSheet";


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



it("shows the project defaults result beside its pinned Save button", async () => {
  const project = app.session.state.get().projects[ids.pTide]!;
  render(<ProjectSheet project={project} onClose={() => {}} />, { wrapper });
  fireEvent.click(screen.getByRole("button", { name: "Save defaults" })); await advance();
  const footer = screen.getByRole("button", { name: "Save defaults" }).closest(".sheet__footer");
  expect(footer?.textContent).toContain("Save project defaults");
  expect(footer?.textContent).toContain("Sent to studio");
});
