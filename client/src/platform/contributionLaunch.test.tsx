// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "../app/App";
import { useApp, useUi } from "../app/context";
import { prefs } from "../app/prefs";
import { FakeLink, platform, profile } from "./lifecycle.fixtures";
import { secondProfile } from "../dev/sim/lifecycle";
import { contributionLink, CONTRIBUTION_DELIVERY_FEATURE } from "./contributionLinks";
import { handoffs, resetHandoffs } from "../screens/contribute/handoffs";
import * as fx from "../dev/sim/scenario";
import type { Query } from "../protocol/types";
import { ScanClankerhub, WaitingHandoffs } from "../screens/contribute/Waiting";

class DeliveryLink extends FakeLink {
  queries: Extract<Query, { kind: "contribution_delivery" }>[] = [];
  replies: (() => void)[] = [];
  held = false;
  delay = 0;
  busyUntil = 0;
  send(frame: Parameters<FakeLink["send"]>[0]) {
    if (frame.type !== "query" || frame.query.kind !== "contribution_delivery") return super.send(frame);
    this.queries.push(frame.query);
    const reply = () => this.emit({ type: "frame", frame: { type: "query_result", query_id: frame.query_id,
      result: { Ok: { kind: "contribution_delivery", handoff_json: ' {"gameId":"fixture"}\n' } } } });
    if (this.held) this.replies.push(reply);
    else if (this.delay) {
      // The real companion handles control queries serially. HostSession's
      // timeout starts at send, including any time waiting behind a fetch.
      this.busyUntil = Math.max(Date.now(), this.busyUntil) + this.delay;
      setTimeout(reply, this.busyUntil - Date.now());
    } else queueMicrotask(reply);
    return true;
  }
}
function supported(): DeliveryLink {
  const original = fx.capabilities();
  vi.spyOn(fx, "capabilities").mockImplementation(() => ({ ...original, features: [...original.features, CONTRIBUTION_DELIVERY_FEATURE] }));
  const link = new DeliveryLink();
  fake.value.control = () => link;
  return link;
}
function launch(offset = 0): { url: string; packet: string } {
  const now = Math.floor(Date.now() / 1000) + offset;
  const packet = ` ${JSON.stringify({ schemaVersion: 1, kind: "contribution-import", packetId: "fixture", issuedAt: now, expiresAt: now + 600, handoffSha256: "a".repeat(64) })}\n`;
  return { packet, url: contributionLink(packet, "https://example.com/handoff.json") };
}

let fake: ReturnType<typeof platform>;
vi.mock("../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../shell/Shell", () => ({ Shell: function TestShell() {
  const app = useApp();
  const route = useUi((s) => s.stacks[s.tab].at(-1)?.screen);
  return <><p>Computer {app.profile.name}</p><p>Route {route}</p><WaitingHandoffs /><ScanClankerhub /></>;
} }));
beforeEach(() => {
  localStorage.clear();
  resetHandoffs();
  prefs.set({ theme: "system", reduceMotion: false, lastProfileId: null, claudeBypassPermissions: true, codexBypassPermissions: true });
  fake = platform();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); });

it("accepts a contribution app link instead of rejecting it as an item link", async () => {
  render(<App />);
  await screen.findByText("Route talk");
  const now = Math.floor(Date.now() / 1000);
  const packet = JSON.stringify({ schemaVersion: 1, kind: "contribution-import", packetId: "fixture", issuedAt: now, expiresAt: now + 600, handoffSha256: "a".repeat(64) });
  const link = `ophio-contribution://open/v1?packet=${btoa(packet).replaceAll("=", "")}&handoff=https%3A%2F%2Fexample.com%2Fhandoff.json`;
  act(() => fake.lifecycle.controls.openLink(link));
  expect(screen.queryByRole("dialog", { name: "Can’t open link" })).toBeNull();
});

it("delivers exact records, opens review, and sends no preparation or task command", async () => {
  const link = supported(), { url, packet } = launch();
  render(<App />); await screen.findByText("Route talk");
  act(() => fake.lifecycle.controls.openLink(url));
  await screen.findByText("Route contribution");
  expect(link.queries).toEqual([{ kind: "contribution_delivery", packet_json: packet, handoff_url: "https://example.com/handoff.json" }]);
  expect(handoffs().get()[0]).toMatchObject({ packetJson: packet, handoffJson: ' {"gameId":"fixture"}\n', started: false });
  expect(link.sent).toEqual([]);
  act(() => fake.lifecycle.controls.openLink(url));
  await waitFor(() => expect(link.queries).toHaveLength(2));
  expect(handoffs().get()).toHaveLength(1);
});

it("retains a launch through locking and waits for local unlock to fetch", async () => {
  const link = supported();
  fake.lifecycle.controls.lock();
  render(<App />); await screen.findByRole("heading", { name: "App locked" });
  act(() => fake.lifecycle.controls.openLink(launch().url));
  expect(link.queries).toEqual([]);
  expect(screen.queryByRole("dialog", { name: "Opening from Clankerhub" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Unlock app" }));
  act(() => fake.lifecycle.controls.authenticate());
  await screen.findByText("Route contribution");
  expect(link.sent).toEqual([]);
});

it("ignores a late delivery after dismissing it", async () => {
  const link = supported(); link.held = true;
  render(<App />); await screen.findByText("Route talk");
  act(() => fake.lifecycle.controls.openLink(launch().url));
  await waitFor(() => expect(link.replies).toHaveLength(1));
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  await act(async () => { link.replies[0]?.(); });
  expect(screen.getByText("Route talk")).toBeTruthy();
  expect(handoffs().get()).toEqual([]);
});

it("restores a pending launch after restart and rejects expiry without a fetch", async () => {
  const link = supported(), { url } = launch(-601);
  localStorage.setItem("contribution-delivery.v1", url);
  render(<App />);
  await screen.findByText("This handoff expired. Open it again in Clankerhub to get a fresh one.");
  expect(link.queries).toEqual([]);
  expect(link.sent).toEqual([]);
});

it("does not send an unknown query to an old companion or a device without Manage", async () => {
  const link = new DeliveryLink(); fake.value.control = () => link;
  render(<App />); await screen.findByText("Route talk");
  act(() => fake.lifecycle.controls.openLink(launch().url));
  await screen.findByText("Update Øphio on this computer to receive Clankerhub handoffs.");
  expect(link.queries).toEqual([]);
  cleanup();
  const denied = supported(); denied.grants.admin = false;
  render(<App />);
  await screen.findByText("This device needs Manage permission on the computer to receive a handoff.");
  expect(denied.queries).toEqual([]);
});

it("uses the camera scan as the desktop fallback and reaches the same review", async () => {
  const link = supported(), { url } = launch();
  const scan = vi.fn(async () => url);
  fake.value.scanPairingCode = scan;
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Scan a Clankerhub code" }));
  // Tasks has no Clankerhub section until a handoff waits.
  expect(screen.queryByRole("heading", { name: "From Clankerhub" })).toBeNull();
  await screen.findByText("Route contribution");
  expect(screen.getByRole("heading", { name: "From Clankerhub" })).toBeTruthy();
  expect(scan).toHaveBeenCalledOnce();
  expect(link.queries).toHaveLength(1);
  expect(link.sent).toEqual([]);
});

it("does not reveal a late response while the app is locked", async () => {
  const link = supported(); link.held = true;
  render(<App />); await screen.findByText("Route talk");
  act(() => fake.lifecycle.controls.openLink(launch().url));
  await waitFor(() => expect(link.replies).toHaveLength(1));
  act(() => fake.lifecycle.controls.lock());
  await act(async () => { link.replies[0]?.(); });
  expect(handoffs().get()).toEqual([]);
  expect(screen.queryByText("Route contribution")).toBeNull();
  expect(localStorage.getItem("contribution-delivery.v1")).not.toBeNull();
});

it.each(["reopen", "lock/unlock"])("coalesces a slow delivery across %s without losing its successful reply", async (action) => {
  const link = supported(); link.delay = 13_000;
  render(<App />); await screen.findByText("Route talk");
  vi.useFakeTimers({ toFake: ["Date", "setTimeout", "clearTimeout"] });
  const { url } = launch();
  act(() => fake.lifecycle.controls.openLink(url));
  await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
  if (action === "reopen") act(() => fake.lifecycle.controls.openLink(url));
  else {
    act(() => fake.lifecycle.controls.lock());
    fireEvent.click(screen.getByRole("button", { name: "Unlock app" }));
    await act(async () => { fake.lifecycle.controls.authenticate(); });
  }
  await act(async () => { await vi.advanceTimersByTimeAsync(26_000); });
  expect(screen.getByText("Route contribution")).toBeTruthy();
  expect(link.queries).toHaveLength(1);
  expect(handoffs().get()).toHaveLength(1);
});

it("serializes different links and skips an obsolete waiting link", async () => {
  const link = supported(); link.delay = 13_000;
  render(<App />); await screen.findByText("Route talk");
  vi.useFakeTimers({ toFake: ["Date", "setTimeout", "clearTimeout"] });
  const first = launch();
  act(() => fake.lifecycle.controls.openLink(first.url));
  await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
  act(() => fake.lifecycle.controls.openLink(launch().url));
  await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
  const latest = launch();
  act(() => fake.lifecycle.controls.openLink(latest.url));
  expect(link.queries).toHaveLength(1);
  await act(async () => { await vi.advanceTimersByTimeAsync(11_000); });
  expect(handoffs().get()).toEqual([]);
  expect(link.queries.map(q => q.packet_json)).toEqual([first.packet, latest.packet]);
  await act(async () => { await vi.advanceTimersByTimeAsync(13_000); });
  expect(screen.getByText("Route contribution")).toBeTruthy();
  expect(handoffs().get().map(h => h.packetJson)).toEqual([latest.packet]);
});

it.each(["dismiss", "lock"])("does not send a waiting link after %s", async (action) => {
  const link = supported(); link.delay = 13_000;
  render(<App />); await screen.findByText("Route talk");
  vi.useFakeTimers({ toFake: ["Date", "setTimeout", "clearTimeout"] });
  act(() => fake.lifecycle.controls.openLink(launch().url));
  await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
  act(() => fake.lifecycle.controls.openLink(launch().url));
  if (action === "dismiss") fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  else act(() => fake.lifecycle.controls.lock());
  await act(async () => { await vi.advanceTimersByTimeAsync(26_000); });
  expect(link.queries).toHaveLength(1);
  expect(handoffs().get()).toEqual([]);
});

it.each(["offline", "outdated", "denied", "busy"])("switches from a computer that is %s without receiving the link again", async (state) => {
  const a = supported(), b = new DeliveryLink(), other = secondProfile(profile);
  if (state === "offline") a.offline = true;
  if (state === "denied") a.grants.admin = false;
  if (state === "busy") a.held = true;
  if (state === "outdated") {
    const caps = fx.capabilities();
    vi.mocked(fx.capabilities).mockReturnValueOnce({ ...caps, features: caps.features.filter(f => f !== CONTRIBUTION_DELIVERY_FEATURE) });
  }
  b.snap.host = { ...b.snap.host, host_id: other.hostId, name: other.name };
  b.held = true;
  fake.value.profiles = async () => [profile, other];
  fake.value.control = id => id === profile.profileId ? a : b;
  render(<App />); await screen.findByText("Computer studio");
  const { url, packet } = launch();
  act(() => fake.lifecycle.controls.openLink(url));
  const dialog = await screen.findByRole("dialog", { name: "Opening from Clankerhub" });
  fireEvent.click(within(dialog).getByRole("button", { name: "Switch to spare" }));
  await screen.findByText("Computer spare");
  await waitFor(() => expect(b.replies).toHaveLength(1));
  expect(localStorage.getItem("contribution-delivery.v1")).toBe(url);
  // A reply from the former computer must not consume the reference or open
  // review on the newly selected computer.
  await act(async () => { a.replies[0]?.(); });
  expect(handoffs().get()).toEqual([]);
  await act(async () => { b.replies[0]?.(); });
  expect(screen.getByText("Route contribution")).toBeTruthy();
  expect(handoffs().get()[0]?.packetJson).toBe(packet);
  expect(a.queries).toHaveLength(state === "busy" ? 1 : 0);
  expect(b.queries).toHaveLength(1);
});
