// SPDX-License-Identifier: Apache-2.0
import { StrictMode, type ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { prefs } from "../../app/prefs";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { RouteProvider } from "../../shell/Screen";
import Desktop from "./Desktop";
import type { Picture } from "./useMedia";

let app: App;
const picture = { current: { at: 0, revision: 1, connected: true, error: null } as Picture };
const advance = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const route = { screen: "desktop" as const, targetId: ids.tgHdmi };
const wrapper = ({ children }: { children: ReactNode }) => (
  <StrictMode>
    <AppProvider app={app}><RouteProvider value={{ tab: "control", route, depth: 1, active: true }}>{children}</RouteProvider></AppProvider>
  </StrictMode>
);
vi.mock("./useMedia", async (importOriginal) => ({
  ...await importOriginal<typeof import("./useMedia")>(),
  useMedia: () => ({ picture: picture.current, latest: picture, canvas: { current: null }, retry: vi.fn(), reopen: vi.fn() }),
}));

beforeEach(async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
  picture.current = { at: Date.now(), revision: app.session.state.get().targets.find((t) => t.target_id === ids.tgHdmi)?.geometry.revision ?? 1, connected: true, error: null };
});
afterEach(async () => {
  cleanup();
  await act(async () => { await Promise.resolve(); });
  app.session.stop();
  window.__sim?.host.dispose();
  prefs.set((p) => ({ ...p, screenNames: {} }));
  vi.clearAllTimers();
  vi.useRealTimers();
});

const label = (id: string) => app.session.state.get().targets.find((t) => t.target_id === id)?.label;

it("renames a screen on this device from its More sheet, and can go back to its number", async () => {
  render(<Desktop route={route} />, { wrapper });
  expect(screen.getByRole("heading", { name: "Screen 2 (HDMI-A-1)" })).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "More" }));
  fireEvent.click(screen.getByRole("button", { name: /Rename this screen/ }));
  const sheet = screen.getByRole("dialog", { name: "Rename Screen 2 (HDMI-A-1)" });
  expect(within(sheet).getByText(/Only this phone uses this name\. The computer calls this screen HDMI-A-1\./)).toBeTruthy();
  const save = within(sheet).getByRole("button", { name: "Save name" });
  expect(save.hasAttribute("disabled")).toBe(true);
  fireEvent.change(within(sheet).getByLabelText(/^Screen name/), { target: { value: "Left monitor" } });
  fireEvent.click(save);
  await advance(0);

  expect(label(ids.tgHdmi)).toBe("Left monitor");
  expect(label(ids.tgDp1)).toBe("Screen 1 (DP-1)");
  expect(screen.getByRole("heading", { name: "Left monitor" })).toBeTruthy();

  // A fresh report from the computer keeps it.
  act(() => app.session.state.set((s) => ({ ...s, targets: s.targets.map((t) => ({ ...t, label: t.native_ref })) })));
  expect(label(ids.tgHdmi)).toBe("Left monitor");

  fireEvent.click(screen.getByRole("button", { name: "More" }));
  fireEvent.click(screen.getByRole("button", { name: /Rename this screen/ }));
  fireEvent.click(within(screen.getByRole("dialog", { name: "Rename Left monitor" })).getByRole("button", { name: "Go back to Screen 2 (HDMI-A-1)" }));
  await advance(0);
  expect(label(ids.tgHdmi)).toBe("Screen 2 (HDMI-A-1)");
});
