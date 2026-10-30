// SPDX-License-Identifier: Apache-2.0
import { StrictMode, type ReactNode } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { RouteProvider } from "../../shell/Screen";
import Windows from "./Windows";
import type { Picture } from "./useMedia";

let app: App;
const picture = { current: { at: 0, revision: 1, connected: true, error: null } as Picture };
const advance = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const route = { screen: "windows" as const, targetId: ids.tgHdmi };
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
  vi.useFakeTimers();
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
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

it("says why window and app actions are off until control is enabled", async () => {
  render(<Windows route={route} />, { wrapper });
  await advance(300);
  expect(screen.getAllByText("Focus, Close and Open work once you enable control below.").length).toBeGreaterThan(0);
  expect((screen.getByRole("button", { name: "Enable control" }) as HTMLButtonElement).disabled).toBe(false);
});

it("says what is holding Enable control back", async () => {
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing host");
  const reason = "Computer control needs the on-computer Stop. Run a desktop notification service that supports actions.";
  host.host.capabilities.input = { state: "temporarily_unavailable", reason };
  act(() => host.emit({ type: "capabilities_changed", capabilities: host.host.capabilities }));
  render(<Windows route={route} />, { wrapper });
  await advance(300);
  expect((screen.getByRole("button", { name: "Enable control" }) as HTMLButtonElement).disabled).toBe(true);
  expect(screen.getByText(reason)).toBeTruthy();
});
