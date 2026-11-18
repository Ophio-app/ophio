// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import type { HostProfile } from "../../host/link";
import { createSimPlatform } from "../../dev/sim/platform";
import { SurfaceActive } from "../../design/surface";
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

it("keeps long computer names in accessible labels, not in repeated button text", () => {
  const long = "Workshop computer in the northern office";
  app = { ...app, profiles: [{ ...current, name: long }, { ...old(current), name: long }] };
  render(<PairedComputers />, { wrapper });
  const [now, before] = cards();
  expect(within(now!).getByRole("button", { name: `Rename ${long}` }).textContent).toBe("Rename");
  expect(within(before!).getByRole("button", { name: `Switch to ${long}` }).textContent).toBe("Switch");
  expect(within(before!).getByRole("button", { name: `Rename ${long}` }).textContent).toBe("Rename");
  expect(within(before!).getByRole("button", { name: `Switch to ${long}` }).getAttribute("aria-describedby")).toBeTruthy();
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

it("keeps a typed name while another screen is in front, and drops the sheet for a computer let go of meanwhile", async () => {
  const tree = (active: boolean) => <AppProvider app={app}><SurfaceActive.Provider value={active}><PairedComputers /></SurfaceActive.Provider></AppProvider>;
  const view = render(tree(true));
  fireEvent.click(within(cards()[1]!).getByRole("button", { name: "Rename studio" }));
  fireEvent.change(screen.getByLabelText("Computer name"), { target: { value: "Old studio" } });
  view.rerender(tree(false));
  expect(document.querySelector("dialog[open]")).toBeNull();
  view.rerender(tree(true));
  expect(screen.getByLabelText<HTMLInputElement>("Computer name").value).toBe("Old studio");

  view.rerender(tree(false));
  app = { ...app, profiles: [current] };
  view.rerender(tree(false));
  view.rerender(tree(true));
  expect(screen.queryByLabelText("Computer name")).toBeNull();
  expect(app.rename).not.toHaveBeenCalled();
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

it("renames a computer that keeps its own name there, for every device, and closes at once", async () => {
  app.session.start();
  await act(async () => { await vi.advanceTimersByTimeAsync(900); });
  render(<PairedComputers />, { wrapper });
  fireEvent.click(within(cards()[0]!).getByRole("button", { name: "Rename studio" }));
  expect(screen.getByText("The new name is kept on studio, so every phone and tablet paired with it shows it.")).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Computer name"), { target: { value: "Workshop" } });
  fireEvent.click(screen.getByRole("button", { name: "Save name" }));
  expect(app.rename).toHaveBeenCalledWith(current.profileId, "Workshop");
  expect(screen.queryByLabelText("Computer name")).toBeNull();
  app.session.stop();
});

it("waits for a computer that keeps its own name to connect before renaming it", async () => {
  app.session.start();
  await act(async () => { await vi.advanceTimersByTimeAsync(900); });
  act(() => window.__sim!.offline());
  await act(async () => { await vi.advanceTimersByTimeAsync(100); });
  render(<PairedComputers />, { wrapper });
  fireEvent.click(within(cards()[0]!).getByRole("button", { name: "Rename studio" }));
  expect(screen.getByText(/^Connect to studio to rename it\./)).toBeTruthy();
  expect((screen.getByLabelText("Computer name") as HTMLInputElement).disabled).toBe(true);
  expect((screen.getByRole("button", { name: "Save name" }) as HTMLButtonElement).disabled).toBe(true);
  app.session.stop();
});

it("the simulated computer goes back to a name nobody chose when given its own", async () => {
  app.session.start();
  await act(async () => { await vi.advanceTimersByTimeAsync(900); });
  const rename = async (name: string) => {
    const done = app.session.command({ command: "rename_computer", name }, { label: "Rename" });
    await act(async () => { await vi.advanceTimersByTimeAsync(900); });
    await act(async () => { await done; });
  };
  await rename("Workshop");
  expect(app.session.state.get().host).toEqual(expect.objectContaining({ name: "Workshop", renamed: true }));
  await rename("studio");
  expect(app.session.state.get().host).toEqual(expect.objectContaining({ name: "studio", renamed: false }));
  // Nothing that turns text around gets in.
  const refused = app.session.command({ command: "rename_computer", name: "stu\u202Eoid" }, { label: "Rename" }).catch((e: Error) => e);
  await act(async () => { await vi.advanceTimersByTimeAsync(900); });
  expect(String(await refused)).toMatch(/direction of text/);
  app.session.stop();
});
