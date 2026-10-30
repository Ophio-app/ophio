// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { HostSession } from "../host/session";
import { createSimPlatform } from "../dev/sim/platform";
import { missingGrants } from "./NotAllowed";
import { Shell } from "./Shell";

let app: App;
const advance = (ms: number) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });

beforeEach(async () => {
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  vi.useFakeTimers();
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(
    profile.profileId,
    platform.control(profile.profileId),
  );
  app = {
    platform,
    profile,
    profiles: [profile],
    session,
    ui: new UiStore(profile.profileId),
    switchTo: vi.fn(),
    pairAnother: vi.fn(),
    rename: vi.fn(),
    forget: async () => undefined,
  };
  session.start();
  await advance(900);
});
afterEach(async () => {
  cleanup();
  await act(async () => {
    await Promise.resolve();
  });
  app.session.stop();
  window.__sim?.host.dispose();
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const front = () =>
  document.querySelector("section.tab:not(.is-hidden) > .route:not(.is-hidden)")?.textContent ?? "";

async function control() {
  // The screen loads lazily, and fake timers don't wait for a module.
  await import("../screens/control/Control");
  render(
    <AppProvider app={app}>
      <Shell />
    </AppProvider>,
  );
  act(() => app.ui.reset("control", []));
  await advance(1500);
  // Terminals are Control's Terminals segment.
  fireEvent.click(screen.getByRole("radio", { name: /^Terminals/ }));
  await advance(500);
}

it("a device that manages access switches a missing grant on where it is missing", async () => {
  window.__sim!.permissions({ terminal: false });
  await advance(3000);
  await control();
  expect(front()).toContain("This device may not open terminals");
  expect(front()).not.toContain("devices allow");
  fireEvent.click(screen.getByRole("switch", { name: "Allow Terminals" }));
  await advance(5000);
  expect(app.session.state.get().grants?.terminal).toBe(true);
  expect(front()).not.toContain("This device may not open terminals");
  expect(front()).not.toContain("Reconnect");
});

it("over a slow link the switch stays off and usable with Changing… beside it until the grant is in force; a refusal says why", async () => {
  window.__sim!.permissions({ terminal: false });
  await advance(3000);
  await control();
  window.__sim!.latency(1500);
  const terminals = () => screen.getByRole("switch", { name: "Allow Terminals" }) as HTMLInputElement;
  const refuse = vi.spyOn(app.session, "command").mockRejectedValueOnce(new Error("Not now."));
  fireEvent.click(terminals());
  await advance(0);
  expect(document.body.textContent).toContain("Couldn’t allow Terminals. Not now.");
  refuse.mockRestore();
  fireEvent.click(terminals());
  await advance(300);
  expect(terminals().checked).toBe(false);
  expect(terminals().disabled).toBe(false);
  expect(front()).toContain("Changing…");
  // A second tap while it is on its way asks nothing more.
  fireEvent.click(terminals());
  await advance(8000);
  expect(app.session.state.get().grants?.terminal).toBe(true);
  expect(front()).not.toContain("This device may not open terminals");
});

it("a device that doesn't manage access is told how to allow it", async () => {
  window.__sim!.permissions({ terminal: false, admin: false });
  await advance(3000);
  await control();
  expect(front()).toContain("This device may not open terminals");
  expect(front()).toContain("devices allow");
  expect(screen.queryByRole("switch", { name: "Allow Terminals" })).toBeNull();
});

it("names the grants the companion's words ask for that are missing, with their pairs left to the computer", () => {
  const grants = { view: true, control_input: false, terminal: true, tasks: true, approvals: true, computer_use: false, library_read: true, library_write: true, transfers: true, admin: true, accounts: false };
  expect(missingGrants("view control computer-use", grants)).toEqual(["control_input", "computer_use"]);
  expect(missingGrants("manage", grants)).toEqual([]);
  expect(missingGrants("accounts", grants)).toEqual(["accounts"]);
  expect(missingGrants("tasks", null)).toEqual(["tasks"]);
});
