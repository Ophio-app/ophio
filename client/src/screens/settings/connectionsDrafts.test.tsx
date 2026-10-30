// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { RouteProvider } from "../../shell/Screen";
import Connections from "./Connections";

let app: App;
const advance = (ms: number) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
const route = { screen: "connections" } as const;
const wrapper = ({ children }: { children: ReactNode }) => (
  <AppProvider app={app}>
    <RouteProvider value={{ tab: "work", depth: 1, active: true, route }}>
      {children}
    </RouteProvider>
  </AppProvider>
);

beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = {
    platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId),
    switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined,
  };
  session.start();
  await advance(900);
});
afterEach(async () => {
  cleanup();
  await act(async () => { await Promise.resolve(); });
  app.session.stop();
  window.__sim?.host.dispose();
  vi.clearAllTimers();
  vi.useRealTimers();
});

it("approves a chat link code typed in lower case as the code the bot showed", async () => {
  // crates/services/src/identity.rs makes codes from "23456789ABCDEFGHJKLMNPQRSTUVWXYZ"
  // and looks them up exactly; the bot shows "Code: ABCD2345".
  const query = vi.spyOn(app.session, "query");
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Link a chat" }));
  fireEvent.change(screen.getByLabelText("Chat link code"), { target: { value: " abcd2345 " } });
  fireEvent.click(screen.getByRole("button", { name: "Check code" }));
  await advance(1000);
  // Approving uses the code as checked, so checking it is where the case matters.
  expect(query).toHaveBeenCalledWith({ kind: "channel_link", link_code: "ABCD2345" });
});

it("a refused chat link code does not turn the confirmed empty chat list into one that wasn't loaded", async () => {
  window.__sim!.host.identities = [];
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  expect(screen.getByText("No chats linked.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Link a chat" }));
  fireEvent.change(screen.getByLabelText("Chat link code"), { target: { value: "000-000" } });
  fireEvent.click(screen.getByRole("button", { name: "Check code" }));
  await advance(1000);
  expect(within(screen.getByRole("dialog")).getByText(/No chat is waiting for this code/)).toBeTruthy();
  // Still connected, and the list the computer reported is still known to be empty.
  expect(app.session.online).toBe(true);
  expect(screen.queryByText("Not loaded. Reconnect to check.")).toBeNull();
  expect(screen.getByText("No chats linked.")).toBeTruthy();
});

it("linking another chat starts from an empty code, not the one just used", async () => {
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Link a chat" }));
  fireEvent.change(screen.getByLabelText("Chat link code"), { target: { value: "123-456" } });
  fireEvent.click(screen.getByRole("button", { name: "Check code" }));
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Approve Robin Park" }));
  await advance(1000);
  expect(screen.getByRole("heading", { name: "Robin Park" })).toBeTruthy();
  expect(screen.queryByRole("dialog")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Link a chat" }));
  // A used code is gone from the computer; approving it again only fails.
  expect((screen.getByLabelText("Chat link code") as HTMLInputElement).value).toBe("");
});

