// SPDX-License-Identifier: Apache-2.0
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Snapshot } from "../protocol/types";
import { profile, snapshot, FakeLink, platform } from "./lifecycle.fixtures";
import { HostSession } from "../host/session";
import { emptyHostState } from "../state/projection";
import * as fx from "../dev/sim/scenario";
import { allGrants } from "../dev/sim/host";
import { secondProfile } from "../dev/sim/lifecycle";
import { EARLIER_OBJECT_LINK_PREFIX, OBJECT_LINK_PREFIX, objectLink, parseObjectLink, pairedComputer, resolveObject, withPairing } from "./links";
import { Handoffs, hasRisk, remainingRisk, switchRisk } from "./switching";
import { LockController } from "./lock";
import { LinkPreview } from "./LinkPreview";
import { cachedState, cacheObservations } from "./offline";
import { Appearance } from "../screens/settings/Appearance";
import { prefs } from "../app/prefs";
import { compatibility, installerWords } from "../screens/settings/compatibility";
import { countdown, expiryRemaining } from "../screens/welcome/countdown";
import { plural } from "../screens/settings/helpers";

beforeEach(() => {
  localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

it("accepts only object identity and a contained Library path", () => {
  for (const kind of ["task", "approval", "attention"] as const) {
    const ref = { hostId: fx.ids.host, kind, id: fx.ids.tFlaky };
    expect(parseObjectLink(objectLink(ref))).toEqual(ref);
  }
  const file = { hostId: fx.ids.host, kind: "file" as const, id: fx.ids.rootNotes, path: "guide/é & #.md" };
  expect(parseObjectLink(objectLink(file))).toEqual(file);
  const base = objectLink({ hostId: fx.ids.host, kind: "task", id: fx.ids.tFlaky });
  for (const bad of [base + "?grant=1", base + "#approve", base.replace("task", "approve"), base.replace(fx.ids.tFlaky, ".."), base.replace("open/", "open@evil/"), base.replace("ophio-object", "https"), base + "/"]) expect(parseObjectLink(bad)).toBeNull();
  // Links made before the product was named still open, with the same checks.
  const earlier = (link: string) => link.replace(OBJECT_LINK_PREFIX, EARLIER_OBJECT_LINK_PREFIX);
  expect(parseObjectLink(earlier(objectLink(file)))).toEqual(file);
  expect(parseObjectLink(earlier(base))).toEqual({ hostId: fx.ids.host, kind: "task", id: fx.ids.tFlaky });
  expect(parseObjectLink(earlier(base) + "#approve")).toBeNull();
  expect(parseObjectLink(earlier(base).replace("/v1/", "/v2/"))).toBeNull();
  for (const path of ["../secrets", "/file", "a//b", "a\\b", "C:/file", "\0"]) expect(parseObjectLink(objectLink({ ...file, path }))).toBeNull();
  expect(parseObjectLink(objectLink(file).split("?")[0] + "?path=%ff")).toBeNull();
  expect(pairedComputer({ hostId: "unknown", kind: "task", id: fx.ids.tFlaky }, [profile])).toBeNull();
});
it("opens an object link in the one pairing a computer has after pairing it again", () => {
  const other = secondProfile(profile), again = { ...profile, profileId: "again", pairedAt: profile.pairedAt + 1 };
  const profiles = withPairing([profile, other], again);
  expect(profiles).toEqual([other, again]);
  expect(pairedComputer({ hostId: profile.hostId, kind: "task", id: fx.ids.tFlaky }, profiles)).toBe(again);
  expect(withPairing([other], again)).toEqual([other, again]);
});
it("asks the paired host and refuses unauthorized, missing and mismatched objects", async () => {
  const l = new FakeLink(), session = new HostSession(profile.profileId, l); session.start();
  const ref = { hostId: fx.ids.host, kind: "task" as const, id: fx.ids.tFlaky };
  expect((await resolveObject(ref, session)).state).toBe("ready");
  session.state.set((s) => ({ ...s, grants: { ...allGrants, tasks: false } }));
  expect((await resolveObject(ref, session)).state).toBe("denied");
  session.state.set((s) => ({ ...s, grants: allGrants }));
  expect((await resolveObject({ ...ref, hostId: "different" }, session)).state).toBe("denied");
  l.snap.tasks = [];
  expect((await resolveObject(ref, session)).state).toBe("missing");
  l.snap.approvals = l.snap.approvals.map((a) => ({ ...a, state: "allowed" }));
  expect((await resolveObject({ hostId: fx.ids.host, kind: "approval", id: fx.ids.aTest }, session)).state).toBe("settled");
  expect(l.sent).toEqual([]); session.stop();
});
it("uses the task pill label in a stopped task link preview", async () => {
  const link = new FakeLink(), session = new HostSession(profile.profileId, link); session.start();
  const task = link.snap.tasks[0]!;
  link.snap.tasks = [{ ...task, state: "interrupted" }];
  const result = await resolveObject({ hostId: fx.ids.host, kind: "task", id: task.task_id }, session);
  expect(result).toMatchObject({ state: "settled", detail: "Task status: Stopped. Opening its receipt won’t restart it." });
  session.stop();
});
it("previews before navigation and never submits an approval", async () => {
  const links: FakeLink[] = [], p = platform(links), opened = vi.fn();
  render(<LinkPreview platform={p.value} profiles={[profile]} link={{ hostId: fx.ids.host, kind: "approval", id: fx.ids.aTest }} onClose={() => {}} onOpen={opened} />);
  await screen.findByRole("button", { name: "Open item" });
  expect(opened).not.toHaveBeenCalled();
  expect(links.flatMap((l) => l.sent)).toEqual([]);
  fireEvent.click(screen.getByRole("button", { name: "Open item" }));
  expect(opened).toHaveBeenCalledWith(profile, { screen: "attention", attentionId: fx.ids.atApproval }, expect.any(HostSession));
  expect(links[0]?.snap.approvals[0]?.state).toBe("pending");
  (opened.mock.calls[0]?.[2] as HostSession).stop();
});
it("reuses the active authenticated session without closing its control connection", async () => {
  const l = new FakeLink(), session = new HostSession(profile.profileId, l); session.start();
  await waitFor(() => expect(session.online).toBe(true));
  const p = platform(), open = vi.fn(); p.value.control = () => { throw new Error("A second control connection is not allowed"); };
  const view = render(<LinkPreview existingSession={session} platform={p.value} profiles={[profile]} link={{ hostId: fx.ids.host, kind: "task", id: fx.ids.tFlaky }} onClose={() => {}} onOpen={open} />);
  await screen.findByRole("button", { name: "Open item" });
  fireEvent.click(screen.getByRole("button", { name: "Open item" }));
  expect(open.mock.calls[0]?.[2]).toBe(session); view.unmount(); expect(l.stopped).toBe(false); session.stop();
});
it("fails closed on native errors; unlock is local and does not replay a pending link or approval", async () => {
  const p = platform(), controller = new LockController(p.value);
  const off = controller.start(); await waitFor(() => expect(controller.get().checking).toBe(false));
  p.lifecycle.controls.lock();
  expect(controller.get().locked).toBe(true);
  const received = vi.fn(); p.value.onAppLink?.(received);
  const ref = objectLink({ hostId: fx.ids.host, kind: "approval", id: fx.ids.aTest });
  p.lifecycle.controls.openLink(ref);
  const approval = snapshot().approvals[0];
  const failed = controller.unlock(); p.lifecycle.controls.authenticate(false); await failed;
  expect(controller.get().locked).toBe(true);
  const unlock = controller.unlock(); p.lifecycle.controls.authenticate(); await unlock;
  expect(controller.get().locked).toBe(false);
  expect(received).toHaveBeenCalledTimes(1);
  expect(approval?.state).toBe("pending"); off();
});
function humanControl(): Snapshot["control"][number] {
  return { target_id: fx.ids.tgDp1, input_state: "human", lease: { lease_id: fx.ids.lease1, target_id: fx.ids.tgDp1, owner: { kind: "human", device_id: fx.ids.device }, generation: 1, boot_epoch: fx.ids.boot, state: "active", granted_at: 1, renewal_window_ms: 15000 }, model_observation: null, private_step: null };
}
it("detects own input, unconfirmed Stop and private steps without borrowing another device’s lease", () => {
  const view = humanControl(), snap = snapshot();
  const s = { ...emptyHostState, control: { [view.target_id]: view }, tasks: Object.fromEntries(snap.tasks.map((t) => [t.task_id, t])) };
  expect(switchRisk(s, {}, profile.deviceId).leases).toHaveLength(1);
  expect(hasRisk(switchRisk(s, {}, "someone-else"))).toBe(false);
  const risk = switchRisk(s, { stop: { id: "stop", body: { command: "stop_task", task_id: fx.ids.tFlaky }, phase: "unknown", label: "Stop", issuedAt: 1, receipt: null, error: null } }, profile.deviceId);
  expect(risk.stoppingTasks).toEqual([fx.ids.tFlaky]);
  expect(remainingRisk(risk, { ...snap, control: [] }, profile.deviceId).stoppingTasks).toEqual([fx.ids.tFlaky]);
  expect(remainingRisk(risk, { ...snap, tasks: snap.tasks.map((t) => ({ ...t, state: "completed" })), control: [] }, profile.deviceId).stoppingTasks).toEqual([]);
  view.private_step = { requested_at: 1, input_stopped: "acknowledged", managed_screenshots_stopped: "acknowledged", uncovered_routes: [], prior_observations_delivered: 0, ready: true };
  expect(switchRisk(s, {}, profile.deviceId).privateTargets).toEqual([view.target_id]);
});
it("retains offline control across restart and only clears after host confirmation or knowing dismissal", async () => {
  const l = new FakeLink(); l.snap.control = [humanControl()];
  const session = new HostSession(profile.profileId, l); session.start();
  await waitFor(() => expect(session.state.get().caughtUp).toBe(true));
  const p = platform(), handoffs = new Handoffs(p.value, [profile, secondProfile(profile)]);
  handoffs.activeProfileId = profile.profileId;
  handoffs.retain(profile, session, switchRisk(session.state.get(), {}, profile.deviceId));
  l.offline = true; l.connect(); await handoffs.release(profile.profileId);
  expect(handoffs.get()).toHaveLength(1);
  const restored = new Handoffs(p.value, [profile]); expect(restored.get()[0]?.risk.leases).toHaveLength(1);
  l.offline = false; l.connect(); await handoffs.release(profile.profileId);
  await waitFor(() => expect(handoffs.get()).toHaveLength(0));
  expect(l.sent).toEqual([{ command: "release_lease", lease_id: fx.ids.lease1 }]);
  expect(l.stopped).toBe(false); session.stop(); handoffs.dispose(); restored.dispose();
});
it("keeps an admitted input request unresolved until its effect is observed", () => {
  const state = { ...emptyHostState };
  const pending = { id: "acquire", body: { command: "take_over" as const, target_id: fx.ids.tgDp1, geometry_revision: 1 }, phase: "accepted" as const, label: "Take over", issuedAt: 1, receipt: null, error: null };
  expect(switchRisk(state, { acquire: pending }, profile.deviceId).pendingInput).toEqual([{ commandId: "acquire", targetId: fx.ids.tgDp1 }]);
  expect(switchRisk({ ...state, commandStages: { acquire: { stage: "observed", detail: null, at: 2 } } }, { acquire: pending }, profile.deviceId).pendingInput).toEqual([]);
});
it("does not mistake a release receipt for revocation and keeps private-step review explicit", async () => {
  const l = new FakeLink(); l.snap.control = [humanControl()]; l.released = false;
  const session = new HostSession(profile.profileId, l); session.start(); await waitFor(() => expect(session.state.get().caughtUp).toBe(true));
  const h = new Handoffs(platform().value, [profile]); h.activeProfileId = profile.profileId;
  h.retain(profile, session, switchRisk(session.state.get(), {}, profile.deviceId));
  await h.release(profile.profileId);
  expect(h.get()[0]?.risk.leases).toHaveLength(1);
  l.snap.control[0]!.private_step = { requested_at: 1, input_stopped: "requested", managed_screenshots_stopped: "requested", uncovered_routes: [], prior_observations_delivered: 0, ready: false };
  const risk = { ...h.get()[0]!.risk, privateTargets: [fx.ids.tgDp1] };
  h.retain(profile, session, risk); l.released = true;
  // The fake removes the lease but retains the private step for review.
  l.snap.control[0]!.lease = null; l.released = false;
  await h.release(profile.profileId);
  expect(l.sent).toContainEqual({ command: "pause_input", target_id: fx.ids.tgDp1 });
  expect(h.get()[0]?.risk.privateTargets).toEqual([fx.ids.tgDp1]);
  expect(h.get()[0]?.detail).toContain("review what was typed"); h.dispose(); session.stop();
});
it("does not interpret a view grant removal as confirmed release", async () => {
  const l = new FakeLink(); l.snap.control = [humanControl()];
  const session = new HostSession(profile.profileId, l); session.start(); await waitFor(() => expect(session.state.get().caughtUp).toBe(true));
  const h = new Handoffs(platform().value, [profile]); h.retain(profile, session, switchRisk(session.state.get(), {}, profile.deviceId));
  session.state.set((s) => ({ ...s, grants: { ...allGrants, view: false } })); l.snap.control = [];
  await h.refresh(profile.profileId); expect(h.get()).toHaveLength(1); h.dismiss(profile.profileId); expect(h.get()).toHaveLength(0); h.dispose();
});
it("caches observations only, partitioned by computer; Appearance applies and persists", async () => {
  const l = new FakeLink(), session = new HostSession(profile.profileId, l); session.start(); await waitFor(() => expect(session.state.get().caughtUp).toBe(true));
  const off = cacheObservations(profile, session); off();
  const cached = cachedState(profile); expect(cached?.tasks[fx.ids.tFlaky]?.title).toBeTruthy(); expect(cached?.grants).toBeNull(); expect(cached?.control).toEqual({}); expect(cached?.approvals).toEqual({});
  expect(cachedState({ ...profile, hostId: "wrong" })).toBeNull(); session.stop();
  render(<Appearance />); fireEvent.click(screen.getByRole("radio", { name: "Dark" }));
  expect(prefs.get().theme).toBe("dark"); expect(document.documentElement.dataset.theme).toBe("dark"); expect(JSON.parse(localStorage.getItem("prefs") ?? "{}").theme).toBe("dark");
});
it("reports compatibility and updates honestly, and formats plurals and monotonic expiry", () => {
  expect(compatibility({ major: 1, minor: 0 }, null, true, "Update the app.")).toContain("Update the app.");
  expect(compatibility({ major: 1, minor: 1 }, { major: 1, minor: 0 }, false)).toContain("Update Øphio on the computer");
  expect(compatibility(null, null, false)).toContain("No protocol");
  expect(installerWords(null)).toContain("outside an app store"); expect(installerWords(null, false)).not.toContain("as below"); expect(installerWords("com.android.vending")).toContain("Google Play");
  expect(plural(1, "draft")).toBe("1 draft"); expect(plural(2, "draft")).toBe("2 drafts");
  expect(countdown(expiryRemaining(61000, 1000, 1000))).toBe("0:59"); expect(expiryRemaining(61000, 1000, 90000)).toBe(0);
});
