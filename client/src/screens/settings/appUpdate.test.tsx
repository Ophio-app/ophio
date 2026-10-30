// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { capabilities } from "../../dev/sim/scenario";
import type { AppUpdateResult, AppUpdater } from "../../host/link";
import type { AppOffer } from "../../protocol/types";
import type { HostState } from "../../state/projection";
import { emptyHostState } from "../../state/projection";
import { parseOpenRoute } from "../../platform/notifications";
import { AppUpdate } from "./AppUpdate";

const offer: AppOffer = { sha256: "a".repeat(64), size: 3 * 1024 * 1024, signers: ["b".repeat(64)], version: "0.2.0" };
let state: HostState;
let online = true;
let results: ((r: AppUpdateResult) => void) | null = null;
const check = vi.fn<AppUpdater["check"]>();
const install = vi.fn<AppUpdater["install"]>();
const updater: AppUpdater = {
  check, install,
  onResult: (listener) => { results = listener; return () => { results = null; }; },
};
let platform: { appUpdate?: AppUpdater } = { appUpdate: updater };
const download = vi.fn();
vi.mock("../../host/transfer", () => ({ download: (...args: unknown[]) => download(...args) }));
vi.mock("../../app/context", () => ({
  useApp: () => ({ platform, session: {}, profile: { profileId: "p1", name: "Desk" } }),
  useHost: <T,>(select: (s: HostState) => T) => select(state),
  useLink: () => ({ state: online ? "online" : "offline" }),
  describeError: (e: { message?: string }) => e.message ?? "Request failed",
}));
const grants = { view: true, tasks: true, admin: false, control_input: false, terminal: false, approvals: true, computer_use: false, library_read: true, library_write: false, transfers: true, accounts: false };
beforeEach(() => {
  online = true;
  platform = { appUpdate: updater };
  state = { ...emptyHostState, capabilities: { ...capabilities(), app_updates: [offer] }, grants };
  check.mockReset().mockResolvedValue({ offer, version: "0.1.0", asks: false });
  install.mockReset().mockResolvedValue(undefined);
  download.mockReset();
});
afterEach(cleanup);

const button = () => screen.getByRole("button", { name: "Update now" });

it("downloads the offered app and hands it to Android", async () => {
  const blob = new Blob([new Uint8Array(8)]);
  download.mockImplementation(async (_ctx, o: { onProgress(r: number, s: number): void }) => {
    o.onProgress(4, 8);
    return { transferId: "t1", sha256: offer.sha256, blob };
  });
  render(<AppUpdate />);
  await waitFor(() => expect(screen.getByText(/Desk has Øphio 0\.2\.0 for this phone/)).toBeTruthy());
  expect(check).toHaveBeenCalledWith([offer]);
  expect(screen.getByText(/Android installs it without asking/)).toBeTruthy();
  fireEvent.click(button());
  await waitFor(() => expect(install).toHaveBeenCalledWith("p1", blob, offer.sha256));
  expect(download.mock.calls[0]![1]).toMatchObject({ source: { kind: "app_update", sha256: offer.sha256 } });
  expect(screen.getByRole("status").textContent).toBe("Installing…");
  act(() => results!({ state: "asking", message: null }));
  expect(screen.getByRole("status").textContent).toBe("Confirm the update in Android’s prompt.");
  act(() => results!({ state: "failed", message: "Android or this phone’s administrator blocked the update." }));
  expect(screen.getByText("Android or this phone’s administrator blocked the update.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Try again" })).toBeTruthy();
});

it("installs nothing the computer sent other than what it offered", async () => {
  download.mockResolvedValue({ transferId: "t1", sha256: "c".repeat(64), blob: new Blob([]) });
  render(<AppUpdate />);
  fireEvent.click(await waitFor(button));
  await waitFor(() => expect(screen.getByText(/sent a different app/)).toBeTruthy());
  expect(install).not.toHaveBeenCalled();
});

it("shows Android's refusal as it words it", async () => {
  download.mockResolvedValue({ transferId: "t1", sha256: offer.sha256, blob: new Blob([]) });
  install.mockRejectedValue("The app arrived damaged. Try again.");
  render(<AppUpdate />);
  fireEvent.click(await waitFor(button));
  await waitFor(() => expect(screen.getByText("The app arrived damaged. Try again.")).toBeTruthy());
});

it("says when the app is up to date or the computer offers nothing", async () => {
  check.mockResolvedValue({ offer: null, version: "0.2.0", asks: false });
  const { unmount } = render(<AppUpdate />);
  await waitFor(() => expect(screen.getByText("This app is up to date with Desk.")).toBeTruthy());
  expect(screen.queryByRole("button")).toBeNull();
  unmount();
  state = { ...state, capabilities: capabilities() };
  render(<AppUpdate />);
  expect(screen.getByText(/Desk doesn’t offer app updates/)).toBeTruthy();
  expect(check).toHaveBeenCalledTimes(1);
});

it("needs the transfers permission and a connection, and shows nothing without an updater", async () => {
  state = { ...state, grants: { ...grants, transfers: false } };
  online = false;
  const { unmount } = render(<AppUpdate />);
  await waitFor(() => expect((button() as HTMLButtonElement).disabled).toBe(true));
  expect(screen.getByText(/needs the File transfers permission/)).toBeTruthy();
  expect(screen.getByText("Offline. Connect to update.")).toBeTruthy();
  unmount();
  platform = {};
  const { container } = render(<AppUpdate />);
  expect(container.textContent).toBe("");
});

it("opens Versions from the update notification", () => {
  expect(parseOpenRoute({ kind: "app_update", profileId: "p1" })).toEqual({ kind: "app_update", profileId: "p1" });
  expect(parseOpenRoute({ kind: "app_update", profileId: "" })).toBeNull();
  expect(parseOpenRoute({ kind: "attention", profileId: "p1", attentionId: "a1" })).toEqual({ kind: "attention", profileId: "p1", attentionId: "a1" });
});
