// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Stages from "./Stages";
import { readStageCache, StageDraftStore } from "./stages";

let app: App;
const route = { screen: "stages", projectId: ids.pTide } as const;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 1, active: true }}>{children}</RouteProvider></AppProvider>;
const commands = () => Object.values(app.session.commands.get()).filter(c => c.body.command === "set_project_stage");
beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing fixture");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start(); await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});
async function open() {
  const view = render(<Stages route={route} />, { wrapper });
  await advance();
  return view;
}
function pick(stage = "Implementation") {
  fireEvent.click(screen.getByRole("button", { name: "Change stage" }));
  fireEvent.click(screen.getByLabelText(stage, { exact: true }));
  fireEvent.change(screen.getByLabelText("Note (optional)"), { target: { value: "Reviewed the scope" } });
  fireEvent.click(screen.getByRole("button", { name: "Review stage" }));
}

it("records the first stage with a device and note only after review and receipt", async () => {
  act(() => window.__sim?.host.stages.unset(ids.pTide));
  await open();
  expect(screen.getByText("Stage not set")).toBeTruthy();
  pick("Planning");
  expect(commands()).toHaveLength(0);
  expect(screen.getByText(/No agent gets new permissions/)).toBeTruthy();
  act(() => window.__sim?.latency(1000));
  fireEvent.click(screen.getByRole("button", { name: "Confirm stage change" }));
  expect(screen.getByRole("button", { name: "Recording…" }).hasAttribute("disabled")).toBe(true);
  expect(screen.queryByText("Saved on studio")).toBeNull();
  await advance(1200);
  expect(screen.getByText("Saved on studio")).toBeTruthy();
  const list = screen.getByRole("list", { name: "Stage history, newest first" });
  expect(within(list).getByText("Reviewed the scope")).toBeTruthy();
  expect(within(list).getByText(/Pixel 9/)).toBeTruthy();
  expect(commands()).toHaveLength(1);
});
it("freezes the reviewed revision, names the current stage on refusal, and preserves the selection for retry", async () => {
  await open(); pick();
  act(() => window.__sim?.host.stages.anotherDevice("review", ids.pTide));
  await advance();
  fireEvent.click(screen.getByRole("button", { name: "Confirm stage change" }));
  await advance();
  const sheet = screen.getByRole("dialog");
  expect(within(sheet).getByText(/The current stage is review/)).toBeTruthy();
  expect((within(sheet).getByLabelText("Implementation") as HTMLInputElement).checked).toBe(true);
  expect((within(sheet).getByLabelText("Note (optional)") as HTMLTextAreaElement).value).toBe("Reviewed the scope");
  fireEvent.click(within(sheet).getByRole("button", { name: "Review stage" }));
  fireEvent.click(within(sheet).getByRole("button", { name: "Confirm stage change" }));
  await advance();
  expect(screen.getByText("Saved on studio")).toBeTruthy();
  expect(commands()).toHaveLength(2);
});
it("drafts survive navigation, remount and a different project without sending", async () => {
  const view = await open();
  fireEvent.click(screen.getByRole("button", { name: "Change stage" }));
  fireEvent.click(screen.getByLabelText("Design"));
  fireEvent.change(screen.getByLabelText("Note (optional)"), { target: { value: "Keep this thought" } });
  view.unmount();
  const other = new StageDraftStore(`${app.profile.profileId}.stages.another.draft`);
  expect(other.get().note).toBe("");
  await open();
  fireEvent.click(screen.getByRole("button", { name: "Change stage" }));
  expect((screen.getByLabelText("Note (optional)") as HTMLTextAreaElement).value).toBe("Keep this thought");
  expect((screen.getByLabelText("Design") as HTMLInputElement).checked).toBe(true);
  expect(commands()).toHaveLength(0);
});
it("keeps observed history offline and disables changes with a reason", async () => {
  const view = await open();
  act(() => window.__sim?.offline());
  view.unmount();
  await open();
  expect(screen.getByText(/Reconnect to change the stage/)).toBeTruthy();
  expect(screen.getByText(/Last known on this device/)).toBeTruthy();
  expect(screen.getByRole("list", { name: "Stage history, newest first" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Change stage" }).hasAttribute("disabled")).toBe(true);
  act(() => window.__sim?.revoke());
  await advance();
  expect(screen.getByText(/Pair it again to change the stage/)).toBeTruthy();
  expect(screen.queryByText(/Reconnect to change the stage/)).toBeNull();
});
it("a lost receipt survives remount and is checked without repeating the command", async () => {
  const view = await open(); pick();
  act(() => window.__sim?.dropNextReceipt());
  fireEvent.click(screen.getByRole("button", { name: "Confirm stage change" }));
  await advance();
  expect(screen.getAllByText("Outcome unknown").length).toBeGreaterThan(0);
  view.unmount();
  await open();
  act(() => window.__sim?.online());
  await advance(1600);
  expect(screen.getByText("Saved on studio")).toBeTruthy();
  expect(commands()).toHaveLength(1);
});
it("permission refusal is inline and leaves the note intact", async () => {
  await open(); pick();
  act(() => { if (window.__sim) window.__sim.host.stages.failChange = true; });
  fireEvent.click(screen.getByRole("button", { name: "Confirm stage change" }));
  await advance();
  expect(screen.getAllByText(/no longer has permission/).length).toBeGreaterThan(0);
  expect((screen.getByLabelText("Note (optional)") as HTMLTextAreaElement).value).toBe("Reviewed the scope");
});
it("a failed history query is visible and cannot create a misleading empty history", async () => {
  act(() => { if (window.__sim) window.__sim.host.stages.failQuery = true; });
  await open();
  expect(screen.getByText("History couldn’t be refreshed")).toBeTruthy();
  expect(screen.queryByText("No stage changes yet")).toBeNull();
  expect(screen.getByRole("button", { name: "Change stage" }).hasAttribute("disabled")).toBe(true);
  act(() => { if (window.__sim) window.__sim.host.stages.failQuery = false; });
  fireEvent.click(screen.getByRole("button", { name: "Try again" })); await advance();
  expect(screen.getByRole("list", { name: "Stage history, newest first" })).toBeTruthy();
});
it("a saved in-flight send becomes unknown after an app restart and storage failure prevents dispatch", async () => {
  const store = new StageDraftStore("restart");
  store.update({ target: "planning", note: "Keep", attempt: { id: "saved-id", phase: "sending", message: "Sending" } });
  expect(new StageDraftStore("restart").get().attempt?.phase).toBe("unknown");
  await open(); pick();
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("full"); });
  fireEvent.click(screen.getByRole("button", { name: "Confirm stage change" }));
  await advance();
  expect(commands()).toHaveLength(0);
  expect(screen.getByText(/This device couldn’t save the draft/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Back to choices" }));
  expect(screen.getByText(/Not saved on this device\./)).toBeTruthy();
  expect(screen.queryByText(/Saved on this device until confirmed/)).toBeNull();
});

it("ignores a corrupt history cache without interpreting it as a stage decision", () => {
  localStorage.setItem("work.corrupt", JSON.stringify({ name: "Project", observedAt: 42, history: { transitions: [null], stages: [], current: null } }));
  expect(readStageCache("corrupt")).toBeNull();
  expect(commands()).toHaveLength(0);
});

it("names the manage grant command and enables stage changes when it arrives", async () => {
  act(() => window.__sim?.permissions({ admin: false })); await advance(1000);
  await open();
  expect(screen.getByRole("button", { name: "Change stage" }).hasAttribute("disabled")).toBe(true);
  expect(screen.getByText(`ophio devices allow ${app.profile.deviceId} manage`)).toBeTruthy();
  act(() => window.__sim?.permissions({ admin: true })); await advance(1200);
  expect(screen.getByRole("button", { name: "Change stage" }).hasAttribute("disabled")).toBe(false);
  expect(screen.queryByText(/This device may not change the project stage/)).toBeNull();
});

it.each(["", "Reviewed on tablet"])("stage review describes only the note actually supplied: %s", async note => {
  await open();
  fireEvent.click(screen.getByRole("button", { name: "Change stage" }));
  fireEvent.click(screen.getByLabelText("Implementation", { exact: true }));
  fireEvent.change(screen.getByLabelText("Note (optional)"), { target: { value: note } });
  fireEvent.click(screen.getByRole("button", { name: "Review stage" }));
  expect(screen.getByText(note ? "Adds this decision and your device, along with your note, to the project’s history." : "Adds this decision and your device to the project’s history.")).toBeTruthy();
});
