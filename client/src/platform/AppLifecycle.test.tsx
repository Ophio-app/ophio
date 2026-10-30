// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { App } from "../app/App";
import { useApp, useHost, useUi } from "../app/context";
import { prefs } from "../app/prefs";
import { FakeLink, platform, profile } from "./lifecycle.fixtures";
import { acceptObjectLink, objectLink } from "./links";
import * as fx from "../dev/sim/scenario";
import { secondProfile } from "../dev/sim/lifecycle";

let fake: ReturnType<typeof platform>;
vi.mock("../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../shell/Shell", () => ({ Shell: function TestShell() {
  const app = useApp();
  const route = useUi((s) => s.stacks[s.tab].at(-1)?.screen);
  const pending = useHost((s) => s.approvals[fx.ids.aTest]?.state);
  return <div><p>Computer {app.profile.name}</p><p>Route {route}</p><p>Approval {pending}</p>
    <button onClick={() => app.switchTo("sim-spare")}>Switch computer</button>
    <button onClick={() => void app.session.command({ command: "resolve_approval", approval_id: fx.ids.aTest, decision: { decision: "allow" } })}>Approve explicitly</button>
  </div>;
} }));
beforeEach(() => {
  localStorage.clear(); prefs.set({ theme: "system", reduceMotion: false, lastProfileId: null, claudeBypassPermissions: true, codexBypassPermissions: true });
  fake = platform();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(cleanup);
it("keeps a pending deep link and approval hidden through local unlock, then requires Open item", async () => {
  const links: FakeLink[] = []; fake = platform(links); fake.lifecycle.controls.lock();
  render(<App />);
  await screen.findByRole("heading", { name: "App locked" });
  act(() => fake.lifecycle.controls.openLink(objectLink({ hostId: fx.ids.host, kind: "approval", id: fx.ids.aTest })));
  expect(screen.queryByText("Approval pending")).toBeNull(); expect(screen.queryByRole("button", { name: "Open item" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Unlock app" }));
  act(() => fake.lifecycle.controls.authenticate());
  await screen.findByRole("button", { name: "Open item" });
  expect(screen.getByText("Approval pending")).toBeTruthy();
  expect(links.flatMap((l) => l.sent)).toEqual([]);
  expect(screen.queryByText("Route attention")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Open item" }));
  await screen.findByText("Route attention");
  expect(links.flatMap((l) => l.sent)).toEqual([]);
});
it("switches immediately without safety state and keeps computer projections separate", async () => {
  const other = secondProfile(profile); fake.value.profiles = async () => [profile, other];
  fake.value.control = (id) => { const l = new FakeLink(); if (id === other.profileId) { l.snap.host = { ...l.snap.host, host_id: other.hostId, name: other.name }; l.snap.tasks = []; l.snap.approvals = []; } return l; };
  render(<App />); await screen.findByText("Computer studio"); fireEvent.click(screen.getByRole("button", { name: "Switch computer" }));
  await screen.findByText("Computer spare"); expect(screen.queryByText("Approval pending")).toBeNull();
});
it("asks before leaving control and retains an offline warning after the explicit switch", async () => {
  const other = secondProfile(profile), links: FakeLink[] = [];
  fake.value.profiles = async () => [profile, other];
  fake.value.control = (id) => {
    const l = new FakeLink(); links.push(l);
    if (id === profile.profileId) l.snap.control = [{ target_id: fx.ids.tgDp1, input_state: "human", lease: { lease_id: fx.ids.lease1, target_id: fx.ids.tgDp1, owner: { kind: "human", device_id: profile.deviceId }, state: "active", generation: 1, boot_epoch: fx.ids.boot, granted_at: 1, renewal_window_ms: 15000 }, model_observation: null, private_step: null }];
    else { l.snap.host = { ...l.snap.host, host_id: other.hostId, name: other.name }; l.snap.approvals = []; }
    return l;
  };
  render(<App />); await screen.findByText("Approval pending");
  fireEvent.click(screen.getByRole("button", { name: "Switch computer" }));
  await screen.findByRole("button", { name: "Leave control and switch" });
  expect(screen.getByText("Computer studio")).toBeTruthy();
  act(() => { const l = links[0]; if (l) { l.offline = true; l.connect(); } });
  fireEvent.click(screen.getByRole("button", { name: "Leave control and switch" }));
  await waitFor(() => expect(screen.getByText("Computer spare")).toBeTruthy());
  expect(screen.getByRole("button", { name: "Stop on studio" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Retry release" })).toBeTruthy();
});
it("opens pairing for a pairing link setup sent to the app, but not for one pasted as an item", async () => {
  const offer = { host_id: "host", host_name: "Desk", cert_sha256: "00".repeat(32), addresses: ["127.0.0.1"], pairing_secret: "fixture",
    expires_at: Date.now() + 60_000, protocol: { major: 1, minor: 0 } };
  fake.value.parsePairingLink = () => ({ offer });
  Element.prototype.scrollTo ??= () => {};
  render(<App />); await screen.findByText("Computer studio");
  act(() => acceptObjectLink("ophio://pair?d=fixture"));
  expect(screen.queryByRole("heading", { name: "Pair with Desk?" })).toBeNull();
  act(() => fake.lifecycle.controls.openLink("ophio://pair?d=fixture"));
  await screen.findByRole("heading", { name: "Pair with Desk?" });
  expect(screen.getByText("A link opened this pairing")).toBeTruthy();
});
it("opens the app's own alert at once from what the phone holds, without fetching everything again", async () => {
  const links: FakeLink[] = []; fake = platform(links);
  let tap: ((n: { id: string; route?: unknown }) => void) | undefined;
  fake.value.onNotificationOpen = (listener) => { tap = listener; return () => {}; };
  render(<App />); await screen.findByText("Approval pending");
  const link = links[0]; if (!link) throw new Error("Missing control link");
  const queries: string[] = [], send = link.send.bind(link);
  link.send = (frame) => { if (frame.type === "query") queries.push(frame.query.kind); return send(frame); };
  act(() => tap?.({ id: "one", route: { kind: "attention", profileId: profile.profileId, attentionId: fx.ids.atApproval } }));
  expect(screen.getByText("Route attention")).toBeTruthy();
  expect(screen.queryByRole("dialog", { name: "Opening alert" })).toBeNull();
  expect(queries).toEqual([]); expect(link.sent).toEqual([]); expect(links).toHaveLength(1);
});
