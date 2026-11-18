// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useState } from "react";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { HostSession } from "../host/session";
import { Sheet } from "../design/Sheet";
import { Shell } from "./Shell";

vi.mock("../screens/routes", () => ({ RouteView: () => {
  const [open, setOpen] = useState(false);
  return <><button onClick={() => setOpen(true)}>Open picture</button><Sheet open={open} title="Picture" onClose={() => setOpen(false)}>Picture</Sheet></>;
} }));

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); });

it.each(["route", "tab"])("closes dialogs in kept screens when the %s changes", (kind) => {
  render(<AppProvider app={app}><Shell /></AppProvider>);
  fireEvent.click(within(document.querySelector<HTMLElement>(".tab:not(.is-hidden)")!).getByRole("button", { name: "Open picture" }));
  expect(document.querySelector("dialog[open]")).not.toBeNull();
  const original = app.ui.get().tab;
  act(() => {
    if (kind === "route") app.ui.push({ screen: "inbox" });
    else app.ui.selectTab(original === "library" ? "work" : "library");
  });
  expect(document.querySelector("dialog[open]")).toBeNull();
  act(() => { if (kind === "route") app.ui.back(); else app.ui.selectTab(original); });
  expect(document.querySelector("dialog[open]")).toBeNull();
});
