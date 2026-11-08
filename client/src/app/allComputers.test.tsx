// SPDX-License-Identifier: Apache-2.0
//
// The app holds a connection to every paired computer, shows one or All,
// remembers which, and renames a computer where it keeps its name.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { useApp, useUi } from "./context";
import { prefs } from "./prefs";
import { showEveryProject } from "./everyProject";
import { deferringUi } from "./onComputer";
import { toast, Toasts } from "../design/Toast";
import { FakeLink, platform, profile } from "../platform/lifecycle.fixtures";
import { secondProfile, thirdProfile } from "../dev/sim/lifecycle";
import * as fx from "../dev/sim/scenario";

const spare = secondProfile(profile), laptop = thirdProfile(profile);
const RENDER_FARM = "0192f7a1-0000-7000-8000-000000000203";
let fake: ReturnType<typeof platform>;
let links: Map<string, FakeLink[]>;
let names: Record<string, string>;
vi.mock("../host", () => ({ loadPlatform: async () => fake.value }));
vi.mock("../shell/Shell", () => ({ Shell: function TestShell() {
  const app = useApp();
  const tab = useUi((s) => s.tab);
  const project = useUi((s) => s.projectId);
  return <div><p>Computer {app.profile.name}</p><p>All {app.all ? "yes" : "no"}</p><p>Tab {tab}</p><p>Project {project}</p>
    <p>Paired {app.profiles.map((p) => p.name).join(", ")}</p>
    <button onClick={() => app.switchTo(spare.profileId)}>Choose spare</button>
    <button onClick={() => app.switchTo(profile.profileId)}>Choose studio</button>
    <button onClick={() => app.showAll?.()}>Choose All</button>
    <button onClick={() => app.ui.selectTab("work")}>Tasks tab</button>
    <button onClick={() => app.openProject?.(spare.profileId, RENDER_FARM)}>Open render-farm</button>
    <button onClick={() => app.openProject?.(spare.profileId, RENDER_FARM, () => showEveryProject(false))}>Pick render-farm</button>
    <button onClick={() => {
      // As "Open in Next up" from All's To-Do: two moves in one tap.
      const there = deferringUi(app.uiOf!(spare.profileId), (act) => app.visit!(spare.profileId, act));
      there.contextProject(RENDER_FARM);
      there.push({ screen: "next_up" }, "work");
    }}>Next up on spare</button>
    <p>Route {useUi((s) => s.stacks[s.tab].at(-1)?.screen)}</p>
    <button onClick={() => void app.rename(profile.profileId, "Workshop")}>Rename studio</button>
    <button onClick={() => void app.rename(laptop.profileId, "Desk laptop")}>Rename laptop</button>
    <button onClick={() => void app.forgetOther?.(spare.profileId).catch((e: Error) => toast(e.message, { tone: "stop" }))}>Forget spare</button>
    <Toasts />
  </div>;
} }));

function control(lease = false) {
  return (id: string) => {
    const link = new FakeLink();
    const p = [profile, spare, laptop].find((x) => x.profileId === id)!;
    if (id !== profile.profileId) { link.snap.host = { ...link.snap.host, host_id: p.hostId, name: p.name }; link.snap.approvals = []; link.snap.tasks = []; }
    if (lease && id === profile.profileId) link.snap.control = [{ target_id: fx.ids.tgDp1, input_state: "human", lease: { lease_id: fx.ids.lease1, target_id: fx.ids.tgDp1, owner: { kind: "human", device_id: profile.deviceId }, state: "active", generation: 1, boot_epoch: fx.ids.boot, granted_at: 1, renewal_window_ms: 15000 }, model_observation: null, private_step: null }];
    links.set(id, [...(links.get(id) ?? []), link]);
    return link;
  };
}
beforeEach(() => {
  localStorage.clear();
  prefs.set({ theme: "system", reduceMotion: false, lastProfileId: null, claudeBypassPermissions: true, codexBypassPermissions: true });
  links = new Map(); names = {};
  fake = platform();
  fake.value.profiles = async () => [profile, spare, laptop].map((p) => ({ ...p, name: names[p.profileId] ?? p.name }));
  fake.value.renameProfile = vi.fn(async (id: string, name: string) => { names[id] = name; return { ...[profile, spare, laptop].find((p) => p.profileId === id)!, name }; });
  fake.value.control = control();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(cleanup);

it("keeps one connection to each paired computer however often the person switches", async () => {
  render(<App />);
  await screen.findByText("Computer studio");
  expect([...links.keys()].sort()).toEqual([laptop.profileId, profile.profileId, spare.profileId].sort());
  for (const name of ["Choose spare", "Choose studio", "Choose spare"]) {
    fireEvent.click(screen.getByRole("button", { name }));
    await screen.findByText(`Computer ${name.slice(7)}`);
  }
  for (const [, opened] of links) { expect(opened).toHaveLength(1); expect(opened[0]!.stopped).toBe(false); }
});

it("remembers All across a restart, and choosing a computer leaves it", async () => {
  prefs.set((p) => ({ ...p, allComputers: true }));
  render(<App />);
  await screen.findByText("All yes");
  fireEvent.click(screen.getByRole("button", { name: "Choose spare" }));
  await screen.findByText("Computer spare");
  expect(screen.getByText("All no")).toBeTruthy();
  expect(prefs.get().allComputers).toBe(false);
  expect(prefs.get().lastProfileId).toBe(spare.profileId);
  fireEvent.click(screen.getByRole("button", { name: "Choose All" }));
  await screen.findByText("All yes");
  expect(prefs.get().allComputers).toBe(true);
});

it("opens another computer's project from All on the tab already shown, and stays in All", async () => {
  render(<App />);
  await screen.findByText("Computer studio");
  fireEvent.click(screen.getByRole("button", { name: "Choose All" }));
  fireEvent.click(screen.getByRole("button", { name: "Tasks tab" }));
  fireEvent.click(screen.getByRole("button", { name: "Open render-farm" }));
  await screen.findByText("Computer spare");
  expect(screen.getByText("All yes")).toBeTruthy();
  expect(screen.getByText("Tab work")).toBeTruthy();
  expect(screen.getByText(`Project ${RENDER_FARM}`)).toBeTruthy();
});

it("asks before leaving control when All opens a project on another computer", async () => {
  fake.value.control = control(true);
  render(<App />);
  await screen.findByText("Computer studio");
  fireEvent.click(screen.getByRole("button", { name: "Choose All" }));
  fireEvent.click(screen.getByRole("button", { name: "Open render-farm" }));
  await screen.findByRole("button", { name: "Leave control and switch" });
  expect(screen.getByText("Computer studio")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Leave control and switch" }));
  await screen.findByText("Computer spare");
  expect(screen.getByText("All yes")).toBeTruthy();
});

it("renames a computer on the computer, at once, for every device", async () => {
  render(<App />);
  await screen.findByText("Computer studio");
  await waitFor(() => expect(links.get(profile.profileId)![0]!.listeners.size).toBeGreaterThan(0));
  await act(async () => { await new Promise((r) => setTimeout(r, 20)); });
  fireEvent.click(screen.getByRole("button", { name: "Rename studio" }));
  // The new name shows on the tap.
  expect(screen.getByText("Computer Workshop")).toBeTruthy();
  await waitFor(() => expect(links.get(profile.profileId)![0]!.sent).toContainEqual({ command: "rename_computer", name: "Workshop" }));
  await waitFor(() => expect(fake.value.renameProfile).toHaveBeenCalledWith(profile.profileId, "Workshop"));
  expect(screen.getByText("Computer Workshop")).toBeTruthy();
});

it("puts the name back and says why when the computer refuses it", async () => {
  fake.value.control = (id) => {
    const link = control()(id);
    const send = link.send.bind(link);
    link.send = (frame) => {
      if (frame.type !== "command" || frame.body.command !== "rename_computer") return send(frame);
      queueMicrotask(() => link.emit({ type: "frame", frame: { type: "receipt", command_id: frame.command_id, status: "rejected", seq: null, result: null, error: { code: "permission_denied", message: "This device can’t rename the computer." } } }));
      return true;
    };
    return link;
  };
  render(<App />);
  await screen.findByText("Computer studio");
  await act(async () => { await new Promise((r) => setTimeout(r, 20)); });
  fireEvent.click(screen.getByRole("button", { name: "Rename studio" }));
  expect(screen.getByText("Computer Workshop")).toBeTruthy();
  await screen.findByText("Computer studio");
  expect((await screen.findAllByText(/studio wasn’t renamed\. This device can’t rename the computer\./)).length).toBeGreaterThan(0);
  expect(fake.value.renameProfile).not.toHaveBeenCalled();
});

it("takes the name a computer's owner gave it there", async () => {
  fake.value.control = (id) => {
    const link = control()(id);
    if (id === laptop.profileId) link.snap.host = { ...link.snap.host, name: "Kitchen laptop", renamed: true };
    return link;
  };
  render(<App />);
  await screen.findByText("Paired studio, spare, Kitchen laptop");
  expect(fake.value.renameProfile).toHaveBeenCalledWith(laptop.profileId, "Kitchen laptop");
});

/** The computer announces a new host record, as after a rename there. */
let seq = 0;
function announce(link: FakeLink, host: Partial<FakeLink["snap"]["host"]>) {
  link.snap.host = { ...link.snap.host, ...host };
  act(() => link.emit({ type: "frame", frame: { type: "event", seq: ++seq, at: Date.now(), source: { kind: "companion" }, provenance: "native", event: { type: "host_updated", host: link.snap.host } } }));
}

it("follows a name it took from the computer back to the computer's own, but keeps one given on this phone", async () => {
  fake.value.control = (id) => {
    const link = control()(id);
    if (id === laptop.profileId) link.snap.host = { ...link.snap.host, name: "Kitchen laptop", renamed: true };
    return link;
  };
  render(<App />);
  await screen.findByText("Paired studio, spare, Kitchen laptop");
  // Another phone puts the laptop back on its hostname: a name nobody chose.
  announce(links.get(laptop.profileId)![0]!, { name: "laptop-hostname", renamed: false });
  await screen.findByText("Paired studio, spare, laptop-hostname");
  expect(fake.value.renameProfile).toHaveBeenLastCalledWith(laptop.profileId, "laptop-hostname");
  // A name the person gives on this phone alone, as for an older computer,
  // stays whatever the computer announces without choosing it.
  const studio = links.get(profile.profileId)![0]!;
  vi.mocked(fake.value.renameProfile).mockClear();
  studio.grants = { ...studio.grants };
  announce(studio, { name: "studio-host", renamed: false });
  await act(async () => { await new Promise((r) => setTimeout(r, 20)); });
  expect(fake.value.renameProfile).not.toHaveBeenCalled();
  expect(screen.getByText("Paired studio, spare, laptop-hostname")).toBeTruthy();
});

it("keeps a computer's new name on screen until the computer says it, writing no older one", async () => {
  fake.value.control = (id) => {
    const link = control()(id);
    if (id === laptop.profileId) link.snap.host = { ...link.snap.host, name: "Kitchen laptop", renamed: true };
    return link;
  };
  render(<App />);
  await screen.findByText("Paired studio, spare, Kitchen laptop");
  vi.mocked(fake.value.renameProfile).mockClear();
  fireEvent.click(screen.getByRole("button", { name: "Rename laptop" }));
  expect(screen.getByText("Paired studio, spare, Desk laptop")).toBeTruthy();
  // The receipt comes before the computer's announcement of the name.
  await waitFor(() => expect(links.get(laptop.profileId)![0]!.sent).toContainEqual({ command: "rename_computer", name: "Desk laptop" }));
  await act(async () => { await new Promise((r) => setTimeout(r, 20)); });
  expect(screen.getByText("Paired studio, spare, Desk laptop")).toBeTruthy();
  announce(links.get(laptop.profileId)![0]!, { name: "Desk laptop", renamed: true });
  await act(async () => { await new Promise((r) => setTimeout(r, 20)); });
  expect(screen.getByText("Paired studio, spare, Desk laptop")).toBeTruthy();
  expect(vi.mocked(fake.value.renameProfile).mock.calls).toEqual([[laptop.profileId, "Desk laptop"]]);
});

it("won't forget another computer while control there is still being handed back", async () => {
  localStorage.setItem("unresolved-computers.v1", JSON.stringify([{ profileId: spare.profileId, since: 1, busy: false, detail: "",
    risk: { leases: [{ leaseId: fx.ids.lease1, targetId: fx.ids.tgDp1 }], privateTargets: [], stoppingTasks: [], pendingInput: [] } }]));
  const forget = vi.fn(async () => {});
  fake.value.forget = forget;
  // The spare still reports this phone's control of its screen.
  fake.value.control = (id) => {
    const link = control()(id);
    if (id === spare.profileId) link.snap.control = [{ target_id: fx.ids.tgDp1, input_state: "human", lease: { lease_id: fx.ids.lease1, target_id: fx.ids.tgDp1, owner: { kind: "human", device_id: profile.deviceId }, state: "active", generation: 1, boot_epoch: fx.ids.boot, granted_at: 1, renewal_window_ms: 15000 }, model_observation: null, private_step: null }];
    return link;
  };
  render(<App />);
  await screen.findByText("Computer studio");
  await act(async () => { await new Promise((r) => setTimeout(r, 50)); });
  fireEvent.click(screen.getByRole("button", { name: "Forget spare" }));
  expect((await screen.findAllByText("Leave control and resolve pending Stop requests before forgetting this computer.")).length).toBeGreaterThan(0);
  expect(forget).not.toHaveBeenCalled();
  expect(screen.getByText("Paired studio, spare, laptop")).toBeTruthy();
});

it("forgets another computer once nothing is left to settle there", async () => {
  const forget = vi.fn(async () => {});
  fake.value.forget = forget;
  render(<App />);
  await screen.findByText("Computer studio");
  fireEvent.click(screen.getByRole("button", { name: "Forget spare" }));
  await screen.findByText("Paired studio, laptop");
  expect(forget).toHaveBeenCalledWith(spare.profileId);
  expect(links.get(spare.profileId)![0]!.stopped).toBe(true);
});

it("makes the moves of one tap in one switch, with one leave step and one release", async () => {
  prefs.set((p) => ({ ...p, warningsOff: true }));
  fake.value.control = control(true);
  render(<App />);
  await screen.findByText("Computer studio");
  fireEvent.click(screen.getByRole("button", { name: "Choose All" }));
  fireEvent.click(screen.getByRole("button", { name: "Next up on spare" }));
  await screen.findByText("Computer spare");
  await screen.findByText("Route next_up");
  expect(screen.getByText(`Project ${RENDER_FARM}`)).toBeTruthy();
  const studio = links.get(profile.profileId)![0]!;
  expect(studio.sent.filter((c) => c.command === "release_lease")).toHaveLength(1);
  prefs.set((p) => ({ ...p, warningsOff: false }));
});

it("keeps All projects when the person stays put at the leave-control question", async () => {
  prefs.set((p) => ({ ...p, allProjects: true }));
  fake.value.control = control(true);
  render(<App />);
  await screen.findByText("Computer studio");
  fireEvent.click(screen.getByRole("button", { name: "Choose All" }));
  fireEvent.click(screen.getByRole("button", { name: "Pick render-farm" }));
  const sheet = await screen.findByRole("dialog", { name: "Leave studio?" });
  fireEvent.click(within(sheet).getByRole("button", { name: /^Close/ }));
  await act(async () => { await new Promise((r) => setTimeout(r, 20)); });
  expect(screen.getByText("Computer studio")).toBeTruthy();
  expect(prefs.get().allProjects).toBe(true);
  // Going ahead, it is left once the project is shown.
  fireEvent.click(screen.getByRole("button", { name: "Pick render-farm" }));
  fireEvent.click(await screen.findByRole("button", { name: "Leave control and switch" }));
  await screen.findByText("Computer spare");
  expect(prefs.get().allProjects).toBe(false);
});
