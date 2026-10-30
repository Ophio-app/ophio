// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import type { Availability } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { stopStore } from "../control/stops";
import Talk from "./Talk";

let app: App;
const tick = (ms = 1500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });

/** A new conversation on a computer whose chat limits are `access`. */
async function newConversation(access: Availability) {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.stubGlobal("crypto", { getRandomValues: crypto.getRandomValues.bind(crypto), randomUUID: crypto.randomUUID.bind(crypto) });
  const platform = createSimPlatform(); const profile = (await platform.profiles())[0]!;
  const sim = window.__sim!.host.host;
  sim.capabilities = { ...sim.capabilities, conversation_access: access };
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick(900);
  app.ui.selectProject(ids.pTide);
  await tick(100);
  const route = { screen: "talk" as const };
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 0, active: true, route }}><Talk route={route} /></RouteProvider></AppProvider>);
  await tick(100);
  fireEvent.click(screen.getByRole("button", { name: "New conversation" }));
  await tick(100);
}
afterEach(() => { cleanup(); stopStore(app.session).dispose(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const why = () => document.querySelector(".talk-new__why")?.textContent;

it("says why a chat's access can't be chosen when the computer needs a setup step", async () => {
  await newConversation({ state: "permission_missing", remedy: "Something on this computer stops bubblewrap" });
  expect(screen.queryByRole("button", { name: /^Change & run/ })).toBeNull();
  expect(why()).toBe("To choose what a chat may reach, this computer needs one more setup step. Run ophio doctor on the computer to see it.");
});

it("says a computer that can't limit chats only lets them read", async () => {
  await newConversation({ state: "unsupported", reason: "Windows can’t keep a conversation to what it may reach yet" });
  expect(why()).toBe("This computer can’t limit what a chat may reach yet, so chats here can only read.");
});

it("says nothing more when the access can be chosen", async () => {
  await newConversation({ state: "available" });
  expect(screen.getByRole("button", { name: /^Change & run/ })).toBeTruthy();
  expect(why()).toBeUndefined();
});
