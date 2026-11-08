// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { allGrants } from "../../dev/sim/host";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import { ReadinessCheck } from "./ReadinessCheck";

let app: App;
const advance = async (ms = 0) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}>{children}</AppProvider>;
const NEEDED = "Device management permission is needed to run checks.";

beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers();
});

it("doesn't claim the permission is missing before the computer has said what this phone may do", async () => {
  // As when the app opens and cannot reach the computer.
  act(() => { app.session.link.set({ state: "offline", since: Date.now(), reason: "Cannot reach the computer.", retryAt: null }); });
  expect(app.session.state.get().grants).toBeNull();
  render(<ReadinessCheck />, { wrapper });
  expect(screen.queryByText(NEEDED)).toBeNull();
  expect(screen.getByText("Reconnect to check the computer.")).toBeTruthy();
});

it("says the permission is needed when the computer withholds it", async () => {
  window.__sim!.permissions({ admin: false });
  app.session.start();
  await advance(900);
  render(<ReadinessCheck />, { wrapper });
  expect(screen.getByText(NEEDED)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Check again" })).toHaveProperty("disabled", true);
});

it("says nothing about the permission when the computer grants it", async () => {
  window.__sim!.permissions({ ...allGrants });
  app.session.start();
  await advance(900);
  render(<ReadinessCheck />, { wrapper });
  expect(screen.queryByText(NEEDED)).toBeNull();
  expect(screen.getByRole("button", { name: "Check again" })).toHaveProperty("disabled", false);
});
