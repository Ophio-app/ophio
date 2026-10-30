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
import Settings from "./Settings";
import { PRODUCT_NAME, PROGRAM_NAME } from "../../brand";

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
});

it("a device that may not manage access sees why and the exact command, not a refusal", async () => {
  window.__sim!.permissions({ admin: false });
  await advance(3000);
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  expect(screen.getByText("This device may not manage access")).toBeTruthy();
  expect(
    screen.getByText(`devices allow ${app.profile.deviceId} manage`, {
      exact: false,
    }),
  ).toBeTruthy();
  expect(screen.queryByText(/given permission for that/)).toBeNull();
  expect(screen.queryByText("No devices have been reported.")).toBeNull();
  expect(screen.getAllByText("Hidden from this device.")).toHaveLength(3);
});

it("a device that may manage access lists paired devices", async () => {
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  expect(screen.queryByText("This device may not manage access")).toBeNull();
  expect(screen.getByRole("heading", { name: "Old tablet" })).toBeTruthy();
});

it("a stale Permissions sheet cannot restore a grant revoked on the computer", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const host = window.__sim!.host;
  const device = host.devices.find(d => d.name === "Old tablet")!;
  device.grants = { ...device.grants, view: true, control_input: true, terminal: false };
  host.emit({ type: "devices_changed", devices: [...host.devices] });
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Permissions for Old tablet" }));
  // Local owner revokes desktop input while this sheet retains its old baseline.
  const revoke = app.session.command({ command: "set_device_grants", device_id: device.device_id, expected: device.grants,
    grants: { ...device.grants, control_input: false } }, { label: "Computer removes input" });
  await advance(1000);
  await revoke;
  expect(host.devices.find(d => d.device_id === device.device_id)!.grants.control_input).toBe(false);
  // The person changes only Terminals. They never re-enable desktop input.
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("checkbox", { name: "Terminals" }));
  fireEvent.click(screen.getByRole("button", { name: "Save permissions" }));
  await advance(1000);
  expect(host.devices.find(d => d.device_id === device.device_id)!.grants.control_input).toBe(false);
  const dialog = within(screen.getByRole("dialog"));
  expect(dialog.getByText("This device's permissions changed while you were editing. Review the current permissions and save again.")).toBeTruthy();
  expect(dialog.getByRole("checkbox", { name: "Terminals" })).toHaveProperty("checked", false);
  fireEvent.click(dialog.getByRole("checkbox", { name: "Terminals" }));
  fireEvent.click(screen.getByRole("button", { name: "Save permissions" }));
  await advance(1000);
  expect(host.devices.find(d => d.device_id === device.device_id)!.grants).toMatchObject({ terminal: true, control_input: false });
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("Accounts is given only at the computer, never from a phone", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const host = window.__sim!.host;
  const old = host.devices.find(d => d.name === "Old tablet")!;
  old.grants = { ...old.grants, accounts: false };
  host.emit({ type: "devices_changed", devices: [...host.devices] });
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Permissions for Old tablet" }));
  const dialog = within(screen.getByRole("dialog"));
  const box = dialog.getByRole("checkbox", { name: "Accounts" });
  expect(box).toHaveProperty("disabled", true);
  expect(box.getAttribute("aria-describedby")).toBe("accounts-grant-note");
  expect(dialog.getByText(`devices allow ${old.device_id} accounts`, { exact: false })).toBeTruthy();
  // A stale or forged request is refused by the computer as well.
  const forged = app.session.command({ command: "set_device_grants", device_id: old.device_id, expected: old.grants,
    grants: { ...old.grants, accounts: true } }, { label: "Give Accounts" }).then(() => null, (e: unknown) => e);
  await advance(1000);
  expect(String(await forged)).toContain("Only the computer can give a device access to Accounts.");
  expect(host.devices.find(d => d.device_id === old.device_id)!.grants.accounts).toBe(false);
});

it("Accounts can be taken away from a device here", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const host = window.__sim!.host;
  const old = host.devices.find(d => d.name === "Old tablet")!;
  old.grants = { ...old.grants, accounts: true };
  host.emit({ type: "devices_changed", devices: [...host.devices] });
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Permissions for Old tablet" }));
  const dialog = within(screen.getByRole("dialog"));
  expect(dialog.queryByText(`devices allow ${old.device_id} accounts`, { exact: false })).toBeNull();
  fireEvent.click(dialog.getByRole("checkbox", { name: "Accounts" }));
  fireEvent.click(screen.getByRole("button", { name: "Save permissions" }));
  await advance(1000);
  expect(host.devices.find(d => d.device_id === old.device_id)!.grants.accounts).toBe(false);
});

it("distinguishes missing offline observations from confirmed empty lists", async () => {
  window.__sim?.offline();
  app.session.state.set(s => ({ ...s, devices: [], identities: [], readiness: null, previews: {}, projects: {} }));
  render(<Connections route={route} />, { wrapper });
  await advance(500);
  expect(screen.getAllByText("Not loaded while offline")).toHaveLength(3);
  expect(screen.queryByText("No devices have been reported.")).toBeNull();
  expect(screen.queryByText("No chats linked.")).toBeNull();
  expect(screen.queryByText("No readiness report yet.")).toBeNull();
});

it("offers explicit revocation beside the stale record, without revoking on open", async () => {
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing simulated host");
  const old = host.devices.find(d => d.name === "Old tablet");
  const current = host.devices.find(d => d.device_id === app.profile.deviceId);
  if (!old || !current) throw new Error("Missing devices");
  old.last_seen_at = null;
  old.paired_at = Date.now() - 20 * 86_400_000;
  expect(current.paired_at).toBeGreaterThan(old.paired_at);
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  expect(screen.getByText(`Not seen since ${new Date(old.paired_at).toLocaleDateString()}`)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Revoke Old tablet" })).toHaveProperty("disabled", false);
  expect(host.devices.find(d => d.device_id === old.device_id)?.revoked_at).toBeNull();
});

it("keeps this device marked and first after a shared device-list update", async () => {
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  const host = window.__sim!.host;
  await act(async () => {
    host.emit({
      type: "devices_changed",
      devices: [...host.devices]
        .reverse()
        .map((d) => ({ ...d, is_current: false })),
    });
    await vi.advanceTimersByTimeAsync(500);
  });
  const names = screen
    .getAllByRole("heading", { level: 3 })
    .map((h) => h.textContent);
  expect(names.indexOf("Pixel 9 · this device")).toBeGreaterThanOrEqual(0);
  expect(names.indexOf("Pixel 9 · this device")).toBeLessThan(
    names.indexOf("Old tablet"),
  );
  expect(names.indexOf("Old tablet")).toBeLessThan(
    names.indexOf("Retired phone"),
  );
});

it.each(["demote", "revoke"])("explains the last-manager refusal when trying to %s this device", async (action) => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  if (action === "demote") {
    fireEvent.click(screen.getByRole("button", { name: "Permissions for Pixel 9" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("checkbox", { name: "Manage access" }));
    fireEvent.click(screen.getByRole("button", { name: "Save permissions" }));
  } else {
    fireEvent.click(screen.getByRole("button", { name: "Revoke Pixel 9" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Revoke Pixel 9" }));
  }
  await advance(1000);
  expect(within(screen.getByRole("dialog")).getByText("Give another device Manage access first")).toBeTruthy();
  expect(app.session.online).toBe(true);
  expect(window.__sim!.host.devices.find((d) => d.device_id === app.profile.deviceId)).toMatchObject({ grants: { admin: true }, revoked_at: null });
});

it("expires settled receipts while retaining unconfirmed requests", async () => {
  const command = { id: "rename-receipt", body: { command: "rename_device" as const, device_id: "device", name: "Tablet" }, label: "Rename a paired device", issuedAt: Date.now(), phase: "accepted" as const, receipt: null, error: null };
  app.session.commands.set({ [command.id]: command });
  render(<Connections route={route} />, { wrapper }); await advance(1000);
  expect(screen.getByText(command.label)).toBeTruthy();
  await advance(61_000);
  expect(screen.queryByText(command.label)).toBeNull();
  act(() => app.session.commands.set({ [command.id]: { ...command, phase: "unknown" } }));
  expect(screen.getByText(command.label)).toBeTruthy();
  act(() => app.session.commands.set({
    [command.id]: { ...command, phase: "unknown" },
    newer: { ...command, id: "newer", label: "Rename another paired device", issuedAt: Date.now() },
  }));
  expect(screen.getByText(command.label)).toBeTruthy();
  expect(screen.getByText("Rename another paired device")).toBeTruthy();
});

it("says when a chat link code has expired or is unknown, and how to get a new one", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Link a chat" }));
  const check = async (code: string) => {
    fireEvent.change(screen.getByLabelText("Chat link code"), { target: { value: code } });
    fireEvent.click(screen.getByRole("button", { name: "Check code" }));
    await advance(1000);
    return within(screen.getByRole("dialog"));
  };
  expect((await check("654-321")).getByText(
    "This code has expired. Send any message to the bot to get a new code, then enter it here.",
  )).toBeTruthy();
  expect((await check("000-000")).getByText(
    "No chat is waiting for this code. Check it, or send any message to the bot to get a new code.",
  )).toBeTruthy();
  expect(screen.queryByText("That request expired.")).toBeNull();
});

it("the owner sees who asked for a code, from which chat and project, before approving it", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const host = window.__sim!.host;
  const [tide] = host.projects;
  const before = host.identities.length;
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Link a chat" }));
  fireEvent.change(screen.getByLabelText("Chat link code"), { target: { value: " 123-456 " } });
  fireEvent.click(screen.getByRole("button", { name: "Check code" }));
  await advance(1000);
  // Checking a code changes nothing on the computer.
  expect(host.identities).toHaveLength(before);
  expect(host.links["123-456"]).toBeTruthy();
  const asked = within(within(screen.getByRole("dialog")).getByRole("article", { name: "Who asked for this code" }));
  expect(asked.getByRole("heading", { name: "Robin Park" })).toBeTruthy();
  expect(asked.getByText("Telegram · user 5550199 · chat -1005550100")).toBeTruthy();
  expect(asked.getByText(`Project: ${tide!.name}`)).toBeTruthy();
  expect(screen.getByText(/The bot tells them whether you approved or refused\./)).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Approve Robin Park" }));
  await advance(1000);
  expect(screen.queryByRole("dialog")).toBeNull();
  const linked = host.identities.find((i) => i.display_name === "Robin Park")!;
  expect(linked).toMatchObject({ external_user: "5550199", external_chat: "-1005550100", project_id: tide!.project_id });
  // The linked chat's card names the user it is linked to; the ids wait behind a summary.
  const card = within(screen.getByRole("heading", { name: "Robin Park" }).closest("article")!);
  expect(card.getByText("Telegram chat")).toBeTruthy();
  expect(card.getByText("User 5550199 · chat -1005550100").closest("details")).toBeTruthy();
});

it("the owner can refuse a code after seeing who asked for it", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const host = window.__sim!.host;
  const before = host.identities.length;
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Link a chat" }));
  fireEvent.change(screen.getByLabelText("Chat link code"), { target: { value: "123-456" } });
  fireEvent.click(screen.getByRole("button", { name: "Check code" }));
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Refuse this code" }));
  await advance(1000);
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(host.links["123-456"]).toBeUndefined();
  expect(host.identities).toHaveLength(before);
  // A refused code is spent too, so the next link starts empty.
  fireEvent.click(screen.getByRole("button", { name: "Link a chat" }));
  expect((screen.getByLabelText("Chat link code") as HTMLInputElement).value).toBe("");
});

it("a computer that can't show who asked for a code offers no blind approval", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const host = window.__sim!.host;
  host.host.capabilities.features = host.host.capabilities.features.filter((f) => f !== "channel_link_review");
  await act(async () => host.emit({ type: "capabilities_changed", capabilities: host.host.capabilities }));
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: "Link a chat" }));
  fireEvent.change(screen.getByLabelText("Chat link code"), { target: { value: "123-456" } });
  const dialog = within(screen.getByRole("dialog"));
  expect(dialog.getByText(`Update ${PRODUCT_NAME} on the computer`)).toBeTruthy();
  expect((dialog.getByRole("button", { name: "Check code" }) as HTMLButtonElement).disabled).toBe(true);
  expect(dialog.queryByRole("button", { name: /^Approve/ })).toBeNull();
});

it("puts the chat link code before the access note when the screen is short for its text", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const size = { innerWidth: window.innerWidth, innerHeight: window.innerHeight };
  Object.defineProperties(window, { innerWidth: { value: 360, configurable: true }, innerHeight: { value: 390, configurable: true } });
  try {
    render(<Connections route={route} />, { wrapper });
    act(() => { window.dispatchEvent(new Event("resize")); });
    await advance(1000);
    fireEvent.click(screen.getByRole("button", { name: "Link a chat" }));
    const sheet = within(screen.getByRole("dialog"));
    const note = sheet.getByText(/The computer decides this identity’s permissions/);
    expect(note.closest("details")?.querySelector("summary")?.textContent).toBe("Access and privacy");
    expect(sheet.getByLabelText("Chat link code")).toBeTruthy();
  } finally {
    Object.defineProperties(window, { innerWidth: { value: size.innerWidth, configurable: true }, innerHeight: { value: size.innerHeight, configurable: true } });
    act(() => { window.dispatchEvent(new Event("resize")); });
  }
});

it("a linked chat shows its project, and a manager chooses, changes and stops using one", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const host = window.__sim!.host;
  const [tide, notes] = host.projects;
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  const card = (name: string) => within(screen.getByRole("heading", { name }).closest("article")!);
  expect(card("Sam Rivers").getByText(`Project: ${tide!.name}`)).toBeTruthy();
  expect(card("Alex Chen").getByText("Project: none")).toBeTruthy();

  fireEvent.click(card("Alex Chen").getByRole("button", { name: "Choose project for Alex Chen" }));
  let dialog = within(screen.getByRole("dialog"));
  expect(dialog.getByRole("heading", { name: "Project for Alex Chen" })).toBeTruthy();
  expect(dialog.queryByRole("button", { name: "Stop using a project" })).toBeNull();
  // A chat without a project opens on no choice, so nothing is chosen for it by default.
  expect((dialog.getByLabelText("Project") as HTMLSelectElement).selectedOptions[0]!.textContent).toBe("Choose a project");
  expect((dialog.getByRole("button", { name: "Use project" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.change(dialog.getByLabelText("Project"), { target: { value: notes!.project_id } });
  fireEvent.click(dialog.getByRole("button", { name: `Use ${notes!.name}` }));
  await advance(1000);
  expect(host.identities.find(i => i.id === "sim-chat-discord")!.project_id).toBe(notes!.project_id);
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(card("Alex Chen").getByText(`Project: ${notes!.name}`)).toBeTruthy();

  fireEvent.click(card("Alex Chen").getByRole("button", { name: "Change project for Alex Chen" }));
  dialog = within(screen.getByRole("dialog"));
  expect((dialog.getByLabelText("Project") as HTMLSelectElement).value).toBe(notes!.project_id);
  expect(dialog.queryByRole("option", { name: "Choose a project" })).toBeNull();
  expect((dialog.getByRole("button", { name: `Use ${notes!.name}` }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(dialog.getByRole("button", { name: "Stop using a project" }));
  await advance(1000);
  expect(host.identities.find(i => i.id === "sim-chat-discord")!.project_id).toBeNull();
  expect(card("Alex Chen").getByText("Project: none")).toBeTruthy();

  act(() => host.emit({ type: "channel_identities_changed", identities: host.identities.map(i =>
    i.id === "sim-chat-telegram" ? { ...i, project_id: "0192f7a1-0000-7000-8000-0000000001ff" } : i) }));
  await advance(100);
  expect(card("Sam Rivers").getByText("Project: no longer on this computer")).toBeTruthy();

  window.__sim!.offline();
  await advance(1000);
  expect((card("Sam Rivers").getByRole("button", { name: "Change project for Sam Rivers" }) as HTMLButtonElement).disabled).toBe(true);
  expect((card("Alex Chen").getByRole("button", { name: "Choose project for Alex Chen" }) as HTMLButtonElement).disabled).toBe(true);
});

it("a linked chat is headed by the person's name, with the chat id only as detail", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  const card = within(screen.getByRole("heading", { name: "Sam Rivers" }).closest("article")!);
  expect(card.getByText("Telegram chat")).toBeTruthy();
  expect(card.getByText("User 5550142 · chat 5550142").closest("details")).toBeTruthy();
  fireEvent.click(card.getByRole("button", { name: "Revoke Sam Rivers" }));
  const dialog = within(screen.getByRole("dialog"));
  expect(dialog.getByText(/^Sam Rivers on Telegram will lose chat access\./)).toBeTruthy();
  expect(dialog.queryByText(/5550142/)).toBeNull();
});

it("a linked chat whose connector needs setup says it can't reach the computer, as Readiness does", async () => {
  render(<Connections route={route} />, { wrapper });
  await advance(1000);
  const card = (name: string) => within(screen.getByRole("heading", { name }).closest("article")!);
  expect(card("Alex Chen").getByText("This chat can’t reach the computer")).toBeTruthy();
  expect(card("Alex Chen").getByText("Discord needs setup on the computer.")).toBeTruthy();
  expect(card("Alex Chen").getByText(`${PROGRAM_NAME} channels token set discord`).tagName).toBe("CODE");
  expect(card("Alex Chen").getByRole("button", { name: "Copy command" })).toBeTruthy();
  expect(card("Sam Rivers").queryByText("This chat can’t reach the computer")).toBeNull();
  expect(screen.getByText("Telegram · ready")).toBeTruthy();
  expect(screen.getByText("Discord · not checked").closest("li")!.textContent).toContain("Needs setup.");
  expect(screen.queryByText(/Not configured/)).toBeNull();

  const host = window.__sim!.host;
  host.host.capabilities.discord = { state: "available" };
  await act(async () => host.emit({ type: "capabilities_changed", capabilities: host.host.capabilities }));
  expect(card("Alex Chen").queryByText("This chat can’t reach the computer")).toBeNull();
});

it("Connections and readiness goes back to the Connections that opened Settings", async () => {
  const settings = { screen: "settings" } as const;
  app.ui.selectTab("work");
  app.ui.push(route);
  app.ui.push(settings);
  render(<Settings route={settings} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("button", { name: /Connections and readiness/ }));
  expect(app.ui.get().stacks.work).toEqual([{ screen: "work" }, route]);
});
