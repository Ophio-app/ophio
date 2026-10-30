// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import type { HostProfile } from "../../host/link";
import { createSimPlatform } from "../../dev/sim/platform";
import { PairedComputers, sharedNames } from "./PairedComputers";

let app: App;
let current: HostProfile;
const old = (p: HostProfile): HostProfile => ({ ...p, profileId: "old", hostId: "old-host", addresses: ["studio.tail1234.ts.net:47101"],
  certSha256: "c29e04b7a1f35d6e8c0b2a4f6d8e1c3b5a7f9e0d2c4b6a8f1e3d5c7b9a0f2e4d" });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}>{children}</AppProvider>;
const cards = () => screen.getAllByRole("heading", { level: 3 }).map((h) => h.parentElement as HTMLElement);

beforeEach(async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.useFakeTimers();
  localStorage.clear();
  const platform = createSimPlatform();
  current = (await platform.profiles())[0]!;
  const session = new HostSession(current.profileId, platform.control(current.profileId));
  app = { platform, profile: current, profiles: [current, old(current)], session, ui: new UiStore(current.profileId),
    switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(async () => {}), forget: async () => undefined };
});
afterEach(() => {
  cleanup();
  window.__sim?.host.dispose();
  vi.useRealTimers();
});

it("tells two computers with one name apart by fingerprint, address and when each was last reached", () => {
  render(<PairedComputers />, { wrapper });
  const [now, before] = cards();
  expect(within(now!).getByText("5F1C 9A0E 7D3B 2A4C 6E8F 0A1B 3C5D 7E9F")).toBeTruthy();
  expect(within(now!).getByText("studio.tail1234.ts.net:47100")).toBeTruthy();
  expect(within(before!).getByText("C29E 04B7 A1F3 5D6E 8C0B 2A4F 6D8E 1C3B")).toBeTruthy();
  expect(within(before!).getByText("studio.tail1234.ts.net:47101")).toBeTruthy();
  expect(within(before!).getByText("Not recorded on this device")).toBeTruthy();
  // Both switch buttons read the same; each points at its own details.
  const change = within(before!).getByRole("button", { name: "Switch to studio" });
  expect(document.getElementById(change.getAttribute("aria-describedby")!)?.textContent).toContain("47101");
});

it("shows only the names when every computer has its own", () => {
  app = { ...app, profiles: [current, { ...old(current), name: "spare" }] };
  render(<PairedComputers />, { wrapper });
  expect(screen.queryByText("Fingerprint")).toBeNull();
  expect(screen.queryByText("Last reached")).toBeNull();
  expect(sharedNames([current, { ...current, profileId: "x", name: " STUDIO " }])).toEqual(new Set([current.profileId, "x"]));
});

it("renames a paired computer on this device", async () => {
  render(<PairedComputers />, { wrapper });
  fireEvent.click(within(cards()[1]!).getByRole("button", { name: "Rename studio" }));
  const field = screen.getByLabelText("Computer name");
  expect((field as HTMLInputElement).value).toBe("studio");
  fireEvent.change(field, { target: { value: "  Old studio " } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save name" })); });
  expect(app.rename).toHaveBeenCalledWith("old", "Old studio");
  expect(screen.queryByLabelText("Computer name")).toBeNull();
});

it("keeps the sheet open and says why when the name isn't saved", async () => {
  app.rename = vi.fn(async () => { throw new Error("Give the computer a name of up to 64 characters."); });
  render(<PairedComputers />, { wrapper });
  fireEvent.click(within(cards()[0]!).getByRole("button", { name: "Rename studio" }));
  fireEvent.change(screen.getByLabelText("Computer name"), { target: { value: "Desk" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save name" })); });
  expect(screen.getByText("Give the computer a name of up to 64 characters.")).toBeTruthy();
  expect(screen.getByLabelText("Computer name")).toBeTruthy();
});
