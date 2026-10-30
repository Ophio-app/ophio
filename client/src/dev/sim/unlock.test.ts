// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Event, Grants, ServerFrame } from "../../protocol/types";
import { allGrants, SimHost, SIM_DESKTOP_PASSWORD } from "./host";
import { createSimPlatform } from "./platform";

let host: SimHost;
let n = 0;
beforeEach(() => { vi.useFakeTimers(); host = new SimHost(); host.controlSim.lock(true); });
afterEach(() => { host.dispose(); vi.useRealTimers(); vi.restoreAllMocks(); });

/** Asks to unlock as a device whose grants are `grants` as the computer settles it. */
async function unlock(password: string, grants: () => Grants = () => allGrants, meanwhile = () => {}) {
  const commandId = `unlock-${++n}`;
  await host.command({ command_id: commandId, issued_at: Date.now(), expires_at: null, expected_revision: null, body: { command: "unlock_desktop", password } }, grants);
  meanwhile();
  await vi.advanceTimersByTimeAsync(200);
  const settled = host.journal.find((j) => j.event.type === "command_progress" && j.event.command_id === commandId)?.event as Extract<Event, { type: "command_progress" }>;
  return settled.error?.code ?? settled.stage;
}
const locked = () => host.control.some((v) => v.desktop_lock === "locked");

it("unlocks only for a device with Control and Manage when the computer settles it", async () => {
  expect(await unlock(SIM_DESKTOP_PASSWORD, () => ({ ...allGrants, admin: false }))).toBe("permission_denied");
  expect(await unlock(SIM_DESKTOP_PASSWORD, () => ({ ...allGrants, control_input: false }))).toBe("permission_denied");
  // Manage taken away after the command arrived but before it settled.
  let grants: Grants = allGrants;
  expect(await unlock(SIM_DESKTOP_PASSWORD, () => grants, () => { grants = { ...allGrants, admin: false }; })).toBe("permission_denied");
  expect(locked()).toBe(true);
  // None of those were tries of the password.
  expect(await unlock("not it")).toBe("permission_denied");
  expect(await unlock(SIM_DESKTOP_PASSWORD)).toBe("observed");
  expect(locked()).toBe(false);
});

it("measures a password in bytes, as the computer does, and doesn't count one too long", async () => {
  expect(await unlock("é".repeat(300))).toBe("invalid_request");
  expect(await unlock("é".repeat(300))).toBe("invalid_request");
  expect(await unlock("é".repeat(256))).toBe("permission_denied");
  expect(await unlock(SIM_DESKTOP_PASSWORD)).toBe("observed");
});

it("keeps counting the wrong password before a right one", async () => {
  expect(await unlock("not it")).toBe("permission_denied");
  expect(await unlock(SIM_DESKTOP_PASSWORD)).toBe("observed");
  host.controlSim.lock(true);
  expect(await unlock("nor this")).toBe("permission_denied");
  expect(await unlock("nor that")).toBe("permission_denied");
  expect(await unlock(SIM_DESKTOP_PASSWORD)).toBe("limit_exceeded");
  expect(locked()).toBe(true);
});

it("turns an unlock away at the door from a device without Manage", async () => {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const control = platform.control(profile.profileId);
  const receipts: ServerFrame[] = [];
  control.listen((e) => { if (e.type === "frame" && e.frame.type === "receipt") receipts.push(e.frame); });
  control.connect();
  window.__sim!.permissions({ admin: false });
  await vi.advanceTimersByTimeAsync(900);
  window.__sim!.control.lock(true);
  expect(control.send({ type: "command", command_id: "unlock-door", issued_at: Date.now(), expires_at: null, expected_revision: null, body: { command: "unlock_desktop", password: SIM_DESKTOP_PASSWORD } })).toBe(true);
  await vi.advanceTimersByTimeAsync(900);
  expect(receipts).toEqual([expect.objectContaining({ command_id: "unlock-door", status: "rejected", error: { code: "permission_denied", message: "This device or chat wasn’t given permission for that" } })]);
  expect(window.__sim!.host.control.some((v) => v.desktop_lock === "locked")).toBe(true);
  control.disconnect();
  window.__sim!.host.dispose();
});
