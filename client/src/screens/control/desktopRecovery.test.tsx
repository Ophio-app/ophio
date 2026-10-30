// SPDX-License-Identifier: Apache-2.0
import { StrictMode, type ReactNode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { SIM_DESKTOP_PASSWORD } from "../../dev/sim/host";
import { commandResult } from "../../host/commandResult";
import { useControl } from "./useControl";
import { RouteProvider } from "../../shell/Screen";
import Desktop from "./Desktop";
import { BottomNav } from "../../shell/BottomNav";
import type { Picture } from "./useMedia";


const reopen = vi.hoisted(() => vi.fn());
let app: App;
const picture = {
  current: { at: 0, revision: 1, connected: true, error: null } as Picture,
};
const advance = (ms: number) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
const wrapper = ({ children }: { children: ReactNode }) => (
  <StrictMode>
    <AppProvider app={app}><RouteProvider value={{ tab: "control", route: { screen: "desktop", targetId: ids.tgHdmi }, depth: 1, active: true }}>{children}</RouteProvider></AppProvider>
  </StrictMode>
);

beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
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
  picture.current = {
    at: Date.now(),
    revision: app.session.state.get().targets.find(t => t.target_id === ids.tgHdmi)?.geometry.revision ?? 1,
    connected: true,
    error: null,
  };
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
  vi.restoreAllMocks();
  Reflect.deleteProperty(document, "visibilityState");
});

vi.mock("./useMedia", async (importOriginal) => ({
  ...await importOriginal<typeof import("./useMedia")>(),
  useMedia: () => ({ picture: picture.current, latest: picture, canvas: { current: null }, retry: vi.fn(), reopen }),
}));
const route = { screen: "desktop" as const, targetId: ids.tgHdmi };

it("clears the navigation ownership dot and claim when access is revoked", async () => {
  render(<><Desktop route={route} /><BottomNav /></>, { wrapper });
  await advance(150);
  fireEvent.click(screen.getByRole("button", { name: "Take over" }));
  await advance(650);
  expect(screen.getByRole("button", { name: "Control, input is held" }).querySelector(".nav__dot")).toBeTruthy();
  act(() => window.__sim?.revoke());
  await advance(150);
  expect(screen.queryByRole("button", { name: "Control, input is held" })).toBeNull();
  expect(screen.getByRole("button", { name: "Control, input state unknown" }).querySelector(".nav__dot")).toBeNull();
});

it.each(["offline", "catching_up"])("labels idle input without unknown while %s", async connection => {
  act(() => {
    app.session.state.set(s => ({ ...s, caughtUp: connection !== "catching_up", control: {} }));
    if (connection === "offline") app.session.link.set({ state: "offline", since: Date.now(), reason: "Offline", retryAt: null });
  });
  render(<BottomNav />, { wrapper });
  expect(screen.getByRole("button", { name: "Control" }).querySelector(".nav__dot")).toBeNull();
  expect(screen.queryByRole("button", { name: /input state unknown/ })).toBeNull();
  act(() => app.session.state.set(s => ({ ...s, control: {
    [ids.tgHdmi]: { target_id: ids.tgHdmi, input_state: "ready", lease: null, model_observation: null, private_step: null },
  } })));
  expect(screen.getByRole("button", { name: "Control" }).querySelector(".nav__dot")).toBeNull();
});

it("offers initial control from the first ready projection without a pause command", async () => {
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  expect((screen.getByRole("button", { name: "Take over" }) as HTMLButtonElement).disabled).toBe(false);
  expect(Object.values(app.session.commands.get())).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: "Take over" }));
  await advance(650);
  expect(app.session.state.get().control[ids.tgHdmi]?.input_state).toBe("human");
});

it("fences a geometry hand-over until a frame at the new revision arrives, preserving the lease", async () => {
  const { result, rerender } = renderHook(() => useControl(ids.tgHdmi, picture, true), { wrapper });
  await advance(150);
  act(() => result.current.arm());
  const receipt = app.session.command({ command: "request_human_lease", target_id: ids.tgHdmi, geometry_revision: 1 }, { label: "Control" });
  await advance(650);
  await commandResult(app.session, await receipt);
  expect(result.current.enabled).toBe(true);
  act(() => window.__sim?.control.geometry(ids.tgHdmi));
  await advance(150);
  expect(result.current.enabled).toBe(false);
  expect(result.current.send({ action: "text", text: "stale" })).toBe(false);
  picture.current = { ...picture.current, at: Date.now(), revision: 2 };
  rerender();
  expect(result.current.enabled).toBe(true);
  expect(result.current.send({ action: "text", text: "current" })).toBe(true);
  expect(window.__sim?.control.inputs().at(-1)?.geometry_revision).toBe(2);
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "release_lease")).toBe(false);
});

it("shows a locked computer with no input controls and pauses its picture", async () => {
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  fireEvent.click(screen.getByRole("button", { name: "Take over" }));
  await advance(650);
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  expect(screen.getByText("Locked on computer")).toBeTruthy();
  expect(screen.getByText("Picture paused while locked")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Take over" })).toBeNull();
  expect(screen.queryByRole("button", { name: /^Keyboard$/ })).toBeNull();
});

it("retains refused text, names missing characters and pastes only after the explicit choice", async () => {
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  fireEvent.click(screen.getByRole("button", { name: "Take over" }));
  await advance(650);
  fireEvent.click(screen.getByRole("button", { name: /^Keyboard$/ }));
  fireEvent.change(screen.getByLabelText("Text for the computer"), { target: { value: "Café 日本語" } });
  act(() => window.__sim?.control.refuseText("é日本語", true));
  fireEvent.click(screen.getByRole("button", { name: "Send text" }));
  await advance(150);
  expect(screen.getByText(/Nothing was typed.*é日本語/)).toBeTruthy();
  expect((screen.getByLabelText("Text for the computer") as HTMLTextAreaElement).value).toBe("Café 日本語");
  expect(screen.getByRole("region", { name: "Saved text" }).textContent).toContain("Your text is kept here");
  expect(window.__sim?.control.inputs().some(e => e.action.action === "paste")).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "Paste instead" }));
  await advance(150);
  expect(window.__sim?.control.inputs().at(-1)?.action).toEqual({ action: "paste", text: "Café 日本語" });
  expect(screen.queryByRole("region", { name: "Saved text" })).toBeNull();
});

it("lets you revise refused text before sending a new action", async () => {
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  fireEvent.click(screen.getByRole("button", { name: "Take over" }));
  await advance(650);
  fireEvent.click(screen.getByRole("button", { name: /^Keyboard$/ }));
  fireEvent.change(screen.getByLabelText("Text for the computer"), { target: { value: "Café" } });
  act(() => window.__sim?.control.refuseText("é", true));
  fireEvent.click(screen.getByRole("button", { name: "Send text" }));
  await advance(150);
  expect(screen.getByRole("button", { name: "Paste instead" })).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Text for the computer"), { target: { value: "Cafe" } });
  expect(screen.queryByRole("button", { name: "Paste instead" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Send text" }));
  await advance(150);
  expect(window.__sim?.control.inputs().at(-1)?.action).toEqual({ action: "text", text: "Cafe" });
  expect((screen.getByLabelText("Text for the computer") as HTMLTextAreaElement).value).toBe("");
});

it("requires an explicit action to reopen a removed portal source", async () => {
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing simulated computer");
  host.targets = host.targets.map(target => target.target_id === ids.tgHdmi
    ? { ...target, capture: { state: "temporarily_unavailable", reason: "Display changed, reopen to continue" } }
    : target);
  host.emit({ type: "targets_changed", targets: host.targets });
  await advance(150);
  render(<Desktop route={route} />, { wrapper });
  expect(reopen).not.toHaveBeenCalled();
  expect(screen.getByText(/This starts a new stream/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Reopen sharing" }));
  expect(reopen).toHaveBeenCalledOnce();
});

it("distinguishes ended portal sharing from revoked device pairing", async () => {
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing simulated computer");
  host.targets = host.targets.map(target => target.target_id === ids.tgHdmi
    ? { ...target, capture: { state: "revoked" }, input: { state: "revoked" } }
    : target);
  host.emit({ type: "targets_changed", targets: host.targets });
  await advance(150);
  render(<Desktop route={route} />, { wrapper });
  expect(screen.getByText(/Enable desktop sharing on the computer to reconnect/)).toBeTruthy();
  expect(screen.queryByText(/Pair this device again/)).toBeNull();
  expect(screen.queryByText("Waiting for picture")).toBeNull();
  expect((screen.getByRole("button", { name: "Take over" }) as HTMLButtonElement).disabled).toBe(true);
  expect(reopen).not.toHaveBeenCalled();
});

it("Leave has one pending state, disables repeated Leave and suppresses local input", async () => {
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  fireEvent.click(screen.getByRole("button", { name: "Take over" }));
  await advance(650);
  act(() => window.__sim?.latency(2_000));
  const before = Object.values(app.session.commands.get()).length;
  fireEvent.click(screen.getByRole("button", { name: "Leave control" }));
  const leaving = screen.getByRole("button", { name: "Leaving control…" }) as HTMLButtonElement;
  expect(leaving.disabled).toBe(true);
  expect(screen.queryByText("You have control")).toBeNull();
  expect(screen.queryByText("Watch · read-only")).toBeNull();
  expect(screen.queryByRole("button", { name: /^Keyboard$/ })).toBeNull();
  fireEvent.click(leaving);
  expect(Object.values(app.session.commands.get())).toHaveLength(before + 1);
  await advance(2_600);
  expect(screen.queryByRole("button", { name: "Leaving control…" })).toBeNull();
  // As on the computer, input stays paused under the ended lease.
  expect(screen.getByText("Agent paused", { selector: ".bar__subtitle" })).toBeTruthy();
  expect((screen.getByRole("button", { name: "Take over" }) as HTMLButtonElement).disabled).toBe(false);
});

it("keeps an unknown desktop lock state distinct from a confirmed lock", async () => {
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing host");
  // A companion observing the lock also reports input unavailable while unknown.
  host.host.capabilities.input = { state: "unknown", reason: "Computer lock state is unknown; control is unavailable until lock observation recovers" };
  act(() => {
    host.emit({ type: "capabilities_changed", capabilities: host.host.capabilities });
    host.control = host.control.map(v => v.target_id === ids.tgHdmi ? { ...v, desktop_lock: "unknown" } : v);
    for (const view of host.control) host.emit({ type: "control_changed", view });
  });
  await advance(150);
  render(<Desktop route={route} />, { wrapper });
  expect(screen.getByText("Desktop state unknown")).toBeTruthy();
  expect(screen.queryByText("Locked on computer")).toBeNull();
  expect(document.querySelector(".picture-surface")).toBeNull();
  expect(screen.queryByRole("button", { name: "Take over" })).toBeNull();
  // Only a computer known to be locked offers its password.
  expect(screen.queryByRole("button", { name: "Unlock with password" })).toBeNull();
  expect(host.controlSim.captureAllowed(ids.tgHdmi)).toBe(false);
});

it("does not fence a computer that reports an unknown lock state with input available", async () => {
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing host");
  // A companion with no lock observation always reports unknown and keeps its input policy.
  act(() => {
    host.control = host.control.map(v => v.target_id === ids.tgHdmi ? { ...v, desktop_lock: "unknown" } : v);
    for (const view of host.control) host.emit({ type: "control_changed", view });
  });
  await advance(150);
  render(<Desktop route={route} />, { wrapper });
  expect(screen.queryByText("Desktop state unknown")).toBeNull();
  expect(screen.queryByText("Picture unavailable")).toBeNull();
  expect(screen.getByRole("button", { name: "Take over" })).toBeTruthy();
  expect(host.controlSim.captureAllowed(ids.tgHdmi)).toBe(true);
});

it("locked Computer use has no picture or handback, and the computer refuses lease requests", async () => {
  const { default: ComputerUse } = await import("./ComputerUse");
  act(() => window.__sim?.control.lock(true, ids.tgDp1));
  await advance(150);
  const task = Object.values(app.session.state.get().tasks).find(t => t.target_id === ids.tgDp1);
  if (!task?.current_run) throw new Error("Missing computer task");
  render(<ComputerUse route={{ screen: "computer_use", taskId: task.task_id }} />, { wrapper });
  await advance(150);
  expect(screen.getByText("Picture paused while locked")).toBeTruthy();
  expect(document.querySelector(".picture-surface")).toBeNull();
  expect((screen.getByRole("button", { name: "Take over" }) as HTMLButtonElement).disabled).toBe(true);
  // One Resume agent, in the footer; its review can't resume while locked.
  expect(screen.queryByRole("region", { name: "Resume the agent" })).toBeNull();
  expect(screen.getAllByRole("button", { name: "Resume agent" })).toHaveLength(1);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  fireEvent.click(screen.getByRole("button", { name: "Resume agent" }));
  await advance(150);
  const review = within(screen.getByRole("dialog", { name: /^Resume the agent on/ }));
  expect((review.getByRole("button", { name: "Resume now" }) as HTMLButtonElement).disabled).toBe(true);
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing host");
  expect(() => host.controlSim.before({ command: "take_over", target_id: ids.tgDp1, geometry_revision: 1 })).toThrow();
  expect(() => host.controlSim.before({ command: "return_to_agent", target_id: ids.tgDp1, run_id: task.current_run as string })).toThrow();
  expect(host.controlSim.captureAllowed(ids.tgDp1)).toBe(false);
  expect(() => host.controlSim.before({ command: "capture_target", target_id: ids.tgDp1 })).toThrow();
  expect(() => host.controlSim.before({ command: "resume_task", task_id: task.task_id, text: null })).toThrow();
  expect(host.controlSim.inputs).toHaveLength(0);
});

it("keeps a refused control request beside its action without claiming ownership", async () => {
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  act(() => window.__sim?.control.rejectLease());
  fireEvent.click(screen.getByRole("button", { name: "Take over" }));
  await advance(650);
  expect(document.querySelector(".control-footer")?.textContent).toContain("Input permission was withdrawn");
  expect(screen.queryByText("You have control")).toBeNull();
  expect(window.__sim?.control.inputs()).toHaveLength(0);
});

it("computer-wide Stop readiness disables Enable control and refuses the simulated lease", async () => {
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing host");
  const reason = "Computer control needs the on-computer Stop. Run a desktop notification service that supports actions.";
  host.host.capabilities.input = { state: "temporarily_unavailable", reason };
  act(() => host.emit({ type: "capabilities_changed", capabilities: host.host.capabilities }));
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  expect((screen.getByRole("button", { name: "Take over" }) as HTMLButtonElement).disabled).toBe(true);
  expect(screen.getByText(reason)).toBeTruthy();
  expect(() => host.controlSim.before({ command: "request_human_lease", target_id: ids.tgHdmi, geometry_revision: 1 })).toThrow();
  fireEvent.click(screen.getByRole("button", { name: "Take over" }));
  expect(host.controlSim.inputs).toHaveLength(0);
});

it("revocation replaces ownership claims and removes input controls", async () => {
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  fireEvent.click(screen.getByRole("button", { name: "Take over" }));
  await advance(650);
  expect(screen.getByText("You have control")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Keyboard$/ }));
  act(() => window.__sim?.revoke());
  await advance(150);
  expect(screen.queryByText("You have control")).toBeNull();
  expect(screen.getByText("Access removed")).toBeTruthy();
  expect(screen.queryByText(/This is the computer’s actual desktop/)).toBeNull();
  expect(screen.queryByRole("button", { name: "Send text" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Leave control" })).toBeNull();
  expect(document.querySelector(".picture-surface")).toBeNull();
});

it("stopped tasks keep an unresolved private-step warning with a review route", async () => {
  const { default: Control } = await import("./Control");
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing host");
  host.computer.unverified = true;
  act(() => host.computer.privateStep(ids.tgDp1, true));
  await advance(650);
  const task = host.tasks.find(t => t.target_id === ids.tgDp1);
  if (!task) throw new Error("Missing task");
  const stop = app.session.command({ command: "stop_task", task_id: task.task_id }, { label: "Stop" });
  await advance(1500);
  await stop;
  render(<Control route={{ screen: "control" }} />, { wrapper });
  await advance(200);
  expect(screen.getByText(/Privacy is not confirmed/)).toBeTruthy();
  expect(screen.getByRole("button", { name: new RegExp(task.title) })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Review private step" }));
  expect(app.ui.route()).toEqual({ screen: "computer_use", taskId: task.task_id });
});

it("typing success requires an injected acknowledgement and is replaced by the next action", async () => {
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  fireEvent.click(screen.getByRole("button", { name: "Take over" }));
  await advance(650);
  fireEvent.click(screen.getByRole("button", { name: /^Keyboard$/ }));
  fireEvent.change(screen.getByLabelText("Text for the computer"), { target: { value: "Hello" } });
  fireEvent.click(screen.getByRole("button", { name: "Send text" }));
  expect(screen.queryByText(/^Typed ·/)).toBeNull();
  expect((screen.getByLabelText("Text for the computer") as HTMLTextAreaElement).value).toBe("Hello");
  await advance(150);
  expect(screen.getByText(/^Typed ·/)).toBeTruthy();
  expect((screen.getByLabelText("Text for the computer") as HTMLTextAreaElement).value).toBe("");
  fireEvent.change(screen.getByLabelText("Text for the computer"), { target: { value: "Again" } });
  fireEvent.click(screen.getByRole("button", { name: "Send text" }));
  expect(screen.queryByText(/^Typed ·/)).toBeNull();
});

it("setup has one capture remedy and a code command, and nothing of computer use this device may not start", async () => {
  const { default: Control } = await import("./Control");
  act(() => { window.__sim?.host.tablet.unavailable(); window.__sim?.control.empty(); window.__sim?.permissions({ computer_use: false, admin: false }); });
  await advance(900);
  render(<Control route={{ screen: "control" }} />, { wrapper });
  await advance(200);
  expect(screen.queryByText("No displays available")).toBeNull();
  expect(document.querySelector("code")?.textContent).toBe("ophio desktop enable");
  expect(screen.queryByText(/start computer use/i)).toBeNull();
  expect(screen.queryByText("No desktop tasks running")).toBeNull();
  expect(screen.queryByText(/permission/)).toBeNull();
});

it("computer-use review renders plain actions without developer tool identifiers", async () => {
  const { default: ComputerReview } = await import("./ComputerReview");
  render(<ComputerReview route={{ screen: "computer_use_review", projectId: ids.pNotes }} />, { wrapper });
  await advance(150);
  const actions = screen.getByText("What the agent can do").closest("section");
  expect(actions?.textContent).toContain("See the screen");
  expect(document.body.textContent).not.toMatch(/observe_target|pointer_action|key_action|type_text|request_human/);
});

it("keeps ownership, Leave in the header, and the keyboard input together", async () => {
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  fireEvent.click(screen.getByRole("button", { name: "Take over" }));
  await advance(650);
  fireEvent.click(screen.getByRole("button", { name: /^Keyboard$/ }));
  expect(screen.getByText("This device holds input")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Leave control" }).closest(".bar")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Send text" }).closest(".screen__footer")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Leave control" }));
  expect(screen.queryByRole("region", { name: "Remote keyboard" })).toBeNull();
});

it("names the actual model and explains picture access in plain sentences", async () => {
  const { default: ComputerUse } = await import("./ComputerUse");
  const host = window.__sim!.host;
  act(() => {
    host.control = host.control.map(v => v.model_observation ? { ...v,
      model_observation: { ...v.model_observation, state: "withdrawn" } } : v);
    for (const view of host.control) host.emit({ type: "control_changed", view });
  });
  const { container } = render(<ComputerUse route={{ screen: "computer_use", taskId: ids.tDash }} />, { wrapper });
  await advance(200);
  expect(container.textContent).toContain("Opus 5.5");
  expect(container.textContent).toContain("New pictures to the agent have stopped.");
  expect(container.textContent).toContain("hasn’t said whether a helper agent works the desktop");
  expect(container.textContent).not.toMatch(/claude-opus-5-5|Observation withdrawn|Withdraw observation|fresh observation|No specialist identity reported/);
});

it("revoked access keeps its explanation in the body without a duplicate sticky footer", async () => {
  render(<Desktop route={{ screen: "desktop", targetId: ids.tgHdmi }} />, { wrapper });
  act(() => window.__sim?.revoke());
  await advance(200);
  expect(screen.getByText("Computer access unavailable")).toBeTruthy();
  expect(document.querySelector(".desktop-screen > .screen__footer")).toBeNull();
});

it("unlocks a locked computer with its password, sent once and not kept", async () => {
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  expect(screen.getAllByText(/Unlock it here with its password/).length).toBeGreaterThan(0);
  fireEvent.click(screen.getByRole("button", { name: "Unlock with password" }));
  const field = () => screen.getByLabelText(/password$/) as HTMLInputElement;
  expect(field().type).toBe("password");
  expect(field().autocomplete).toBe("current-password");
  fireEvent.change(field(), { target: { value: "not it" } });
  fireEvent.click(screen.getByRole("button", { name: "Unlock" }));
  expect(field().value).toBe("");
  await advance(400);
  expect(screen.getByText("That isn’t the computer’s password").getAttribute("role")).toBe("alert");
  expect(screen.getByText("Picture paused while locked")).toBeTruthy();
  fireEvent.change(field(), { target: { value: SIM_DESKTOP_PASSWORD } });
  fireEvent.submit(field().closest("form")!);
  await advance(400);
  expect(screen.queryByText("Picture paused while locked")).toBeNull();
  expect(screen.queryByRole("button", { name: "Unlock with password" })).toBeNull();
  const sent = Object.values(app.session.commands.get()).filter((c) => c.body.command === "unlock_desktop");
  expect(sent).toHaveLength(2);
  expect(JSON.stringify(sent)).not.toContain(SIM_DESKTOP_PASSWORD);
  expect(JSON.stringify(sent)).not.toContain("not it");
  expect(JSON.stringify({ ...localStorage })).not.toContain(SIM_DESKTOP_PASSWORD);
});

it("pauses unlocking from here after three wrong passwords, as the computer does", async () => {
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  fireEvent.click(screen.getByRole("button", { name: "Unlock with password" }));
  const field = () => screen.getByLabelText(/password$/) as HTMLInputElement;
  for (const typed of ["not it", "nor this", "nor that", SIM_DESKTOP_PASSWORD]) {
    fireEvent.change(field(), { target: { value: typed } });
    fireEvent.submit(field().closest("form")!);
    await advance(400);
  }
  expect(screen.getByText(/Unlocking from Ophio is paused for 15 min/).getAttribute("role")).toBe("alert");
  expect(screen.getByText("Picture paused while locked")).toBeTruthy();
});

it("asks for Manage before a computer's password can unlock it from here", async () => {
  act(() => window.__sim?.permissions({ admin: false }));
  await advance(900);
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  expect(screen.queryByRole("button", { name: "Unlock with password" })).toBeNull();
  expect(screen.getByText("This device may not unlock the computer")).toBeTruthy();
});

it("offers unlocking on the Control screen itself once the computer says it is locked", async () => {
  const { default: Control } = await import("./Control");
  render(<Control route={{ screen: "control" }} />, { wrapper });
  await advance(150);
  const notice = `${app.profile.name} is locked`;
  expect(screen.queryByText(notice)).toBeNull();
  expect(screen.queryByRole("button", { name: "Unlock with password" })).toBeNull();
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  expect(screen.getByText(notice)).toBeTruthy();
  expect(screen.getByText(/Unlock it here with its password/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Unlock with password" }));
  const field = () => screen.getByLabelText(/password$/) as HTMLInputElement;
  fireEvent.change(field(), { target: { value: SIM_DESKTOP_PASSWORD } });
  fireEvent.submit(field().closest("form")!);
  await advance(400);
  expect(screen.queryByText(notice)).toBeNull();
  expect(screen.queryByRole("button", { name: "Unlock with password" })).toBeNull();
});

it("offers to unlock and start computer use when only the lock is in the way", async () => {
  const { default: Control } = await import("./Control");
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing host");
  render(<Control route={{ screen: "control" }} />, { wrapper });
  await advance(150);
  act(() => { app.ui.selectTab("control"); app.ui.selectProject(ids.pNotes); });
  const start = { name: "Unlock with password and start computer use" };
  expect(screen.queryByRole("button", start)).toBeNull();
  act(() => {
    window.__sim?.control.lock(true);
    host.host.capabilities.computer_use = { state: "temporarily_unavailable", reason: "Computer is locked" };
    host.emit({ type: "capabilities_changed", capabilities: host.host.capabilities });
  });
  await advance(150);
  const section = within(document.querySelector<HTMLElement>(".control-screen")!);
  expect(section.queryByText(/Managed computer use/)).toBeNull();
  expect(section.queryByText("No desktop tasks running")).toBeNull();
  expect(section.queryByRole("button", { name: /^Start computer use/ })).toBeNull();
  fireEvent.click(screen.getByRole("button", start));
  const field = () => section.getByLabelText(/password$/) as HTMLInputElement;
  fireEvent.change(field(), { target: { value: SIM_DESKTOP_PASSWORD } });
  fireEvent.submit(field().closest("form")!);
  await advance(400);
  expect(app.ui.get().stacks.control.at(-1)).toEqual({ screen: "computer_use_review", projectId: ids.pNotes });
});

it("shows nothing of computer use when unlocking would not let it start", async () => {
  const { default: Control } = await import("./Control");
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing host");
  render(<Control route={{ screen: "control" }} />, { wrapper });
  act(() => {
    app.ui.selectProject(ids.pNotes);
    window.__sim?.control.lock(true);
    host.host.capabilities.computer_use = { state: "unsupported", reason: "Install computer-mcp" };
    host.emit({ type: "capabilities_changed", capabilities: host.host.capabilities });
  });
  await advance(150);
  expect(screen.getByRole("button", { name: "Unlock with password" })).toBeTruthy();
  expect(screen.queryByText(/start computer use/i)).toBeNull();
  expect(screen.queryByText(/Managed computer use/)).toBeNull();
});

it("says to unlock at the computer on the Control screen when it can't be unlocked from here", async () => {
  const { default: Control } = await import("./Control");
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing host");
  host.host.capabilities.unlock_session = { state: "unsupported", reason: "Not on this computer" };
  act(() => host.emit({ type: "capabilities_changed", capabilities: host.host.capabilities }));
  render(<Control route={{ screen: "control" }} />, { wrapper });
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  expect(screen.getByText(`${app.profile.name} is locked`)).toBeTruthy();
  expect(screen.getByText(/Unlock it at the computer/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Unlock with password" })).toBeNull();
});

it("tells a device without Manage why the Control screen can't unlock the computer", async () => {
  const { default: Control } = await import("./Control");
  act(() => window.__sim?.permissions({ admin: false }));
  await advance(900);
  render(<Control route={{ screen: "control" }} />, { wrapper });
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  expect(screen.getByText(/Unlock it at the computer/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Unlock with password" })).toBeNull();
  expect(screen.getByText("This device may not unlock the computer")).toBeTruthy();
});

/** Control and a display's screen, mounted together as the shell keeps them,
 * with one of them on screen. */
async function controlAndDisplay() {
  const { default: Control } = await import("./Control");
  const both = (onControl: boolean) => (
    <>
      <RouteProvider value={{ tab: "control", route: { screen: "control" }, depth: 0, active: onControl }}>
        <div data-testid="control"><Control route={{ screen: "control" }} /></div>
      </RouteProvider>
      <RouteProvider value={{ tab: "control", route, depth: 1, active: !onControl }}>
        <div data-testid="display"><Desktop route={route} /></div>
      </RouteProvider>
    </>
  );
  const view = render(both(true), { wrapper });
  return {
    show: (onControl: boolean) => view.rerender(both(onControl)),
    control: () => within(screen.getByTestId("control")),
    display: () => within(screen.getByTestId("display")),
  };
}

const unlocksSent = () =>
  Object.values(app.session.commands.get()).filter((c) => c.body.command === "unlock_desktop");

it("drops a password typed but not sent when its screen is left or put away, or unlocking stops being allowed", async () => {
  const { show, control, display } = await controlAndDisplay();
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing host");
  await advance(150);
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  const field = () => control().getByLabelText(/password$/) as HTMLInputElement;
  const typeHalf = () => {
    fireEvent.click(control().getByRole("button", { name: "Unlock with password" }));
    expect(field().value).toBe("");
    fireEvent.change(field(), { target: { value: "half typed" } });
  };
  const putAway = (state: DocumentVisibilityState) =>
    act(() => {
      Object.defineProperty(document, "visibilityState", { configurable: true, value: state });
      document.dispatchEvent(new Event("visibilitychange"));
    });

  typeHalf();
  show(false);
  expect(control().queryByLabelText(/password$/)).toBeNull();
  expect(display().queryByLabelText(/password$/)).toBeNull();
  expect(display().getByRole("button", { name: "Unlock with password" })).toBeTruthy();
  show(true);
  expect(control().queryByLabelText(/password$/)).toBeNull();

  typeHalf();
  putAway("hidden");
  expect(control().queryByLabelText(/password$/)).toBeNull();
  putAway("visible");
  expect(control().queryByLabelText(/password$/)).toBeNull();

  const capability = host.host.capabilities.unlock_session;
  const gates: [() => void, () => void][] = [
    [() => window.__sim?.permissions({ control_input: false }), () => window.__sim?.permissions({ control_input: true })],
    [() => window.__sim?.permissions({ admin: false }), () => window.__sim?.permissions({ admin: true })],
    [
      () => {
        host.host.capabilities.unlock_session = { state: "unsupported", reason: "Not on this computer" };
        host.emit({ type: "capabilities_changed", capabilities: host.host.capabilities });
      },
      () => {
        host.host.capabilities.unlock_session = capability;
        host.emit({ type: "capabilities_changed", capabilities: host.host.capabilities });
      },
    ],
  ];
  for (const [close, reopen] of gates) {
    typeHalf();
    act(close);
    await advance(900);
    expect(control().queryByLabelText(/password$/)).toBeNull();
    act(reopen);
    await advance(900);
    typeHalf();
    fireEvent.click(control().getByRole("button", { name: "Cancel" }));
  }
  expect(unlocksSent()).toHaveLength(0);
});

it("shows an unlock asked for on one screen as it is checked on another, which sends no second password", async () => {
  const { show, control, display } = await controlAndDisplay();
  await advance(150);
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  fireEvent.click(control().getByRole("button", { name: "Unlock with password" }));
  const asked = control().getByLabelText(/password$/) as HTMLInputElement;
  fireEvent.change(asked, { target: { value: "not it" } });
  fireEvent.submit(asked.closest("form")!);
  show(false);
  const field = () => display().getByLabelText(/password$/) as HTMLInputElement;
  expect(display().queryByRole("button", { name: "Unlock with password" })).toBeNull();
  expect(field().disabled).toBe(true);
  expect(field().value).toBe("");
  const unlock = display().getByRole("button", { name: "Unlock" }) as HTMLButtonElement;
  expect(unlock.disabled).toBe(true);
  expect(unlock.getAttribute("aria-busy")).toBe("true");
  fireEvent.click(unlock);
  fireEvent.submit(field().closest("form")!);
  expect(unlocksSent()).toHaveLength(1);
  await advance(400);
  expect(display().getByText("That isn’t the computer’s password").getAttribute("role")).toBe("alert");
  expect(field().disabled).toBe(false);
  expect(unlocksSent()).toHaveLength(1);
  fireEvent.change(field(), { target: { value: SIM_DESKTOP_PASSWORD } });
  fireEvent.submit(field().closest("form")!);
  await advance(400);
  expect(unlocksSent()).toHaveLength(2);
  expect(display().queryByText("Picture paused while locked")).toBeNull();
  show(true);
  expect(control().queryByText(`${app.profile.name} is locked`)).toBeNull();
});

it.each([
  ["Control", { control_input: false }],
  ["Manage", { admin: false }],
])("offers unlocking with a password on neither screen to a device without %s", async (_, refused) => {
  act(() => window.__sim?.permissions(refused));
  await advance(900);
  const { show, control, display } = await controlAndDisplay();
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  expect(control().getByText(/Unlock it at the computer/)).toBeTruthy();
  expect(control().queryByRole("button", { name: "Unlock with password" })).toBeNull();
  show(false);
  await advance(150);
  expect(display().queryAllByText(/with its password/)).toHaveLength(0);
  expect(display().getAllByText("Unlock on the computer to watch and control again.").length).toBeGreaterThan(0);
  expect(display().queryByRole("button", { name: "Unlock with password" })).toBeNull();
});

it("forgets why the last unlock failed once the computer is unlocked at the computer", async () => {
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  fireEvent.click(screen.getByRole("button", { name: "Unlock with password" }));
  const field = () => screen.getByLabelText(/password$/) as HTMLInputElement;
  fireEvent.change(field(), { target: { value: "not it" } });
  fireEvent.submit(field().closest("form")!);
  await advance(400);
  expect(screen.getByText("That isn’t the computer’s password")).toBeTruthy();
  act(() => window.__sim?.control.lock(false));
  await advance(150);
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  expect(screen.queryByText("That isn’t the computer’s password")).toBeNull();
  expect(screen.getByRole("button", { name: "Unlock with password" })).toBeTruthy();
});

it.each([
  ["before", 0, SIM_DESKTOP_PASSWORD],
  ["after", 130, SIM_DESKTOP_PASSWORD],
  ["before", 0, "not it"],
  ["after", 130, "not it"],
])("keeps an unlock checked through a connection dropped %s its receipt, sending no other password until the computer answers", async (_, receiptAfter, typed) => {
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing host");
  host.unlockCheckMs = 2_000;
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  fireEvent.click(screen.getByRole("button", { name: "Unlock with password" }));
  const field = () => screen.getByLabelText(/password$/) as HTMLInputElement;
  const unlock = () => screen.getByRole("button", { name: "Unlock" });
  fireEvent.change(field(), { target: { value: typed } });
  fireEvent.submit(field().closest("form")!);
  await advance(receiptAfter);
  act(() => window.__sim?.offline());
  await advance(100);
  expect(field().disabled).toBe(true);
  expect(unlock().getAttribute("aria-busy")).toBe("true");
  act(() => window.__sim?.online());
  await advance(900);
  expect(app.session.online).toBe(true);
  expect(screen.getByText("Picture paused while locked")).toBeTruthy();
  expect(field().disabled).toBe(true);
  expect(unlock().getAttribute("aria-busy")).toBe("true");
  fireEvent.change(field(), { target: { value: "again" } });
  fireEvent.submit(field().closest("form")!);
  expect(unlocksSent()).toHaveLength(1);
  await advance(1_500);
  if (typed === SIM_DESKTOP_PASSWORD) {
    expect(screen.queryByText("Picture paused while locked")).toBeNull();
  } else {
    expect(screen.getByText("That isn’t the computer’s password").getAttribute("role")).toBe("alert");
    expect(field().disabled).toBe(false);
  }
  expect(unlocksSent()).toHaveLength(1);
  expect(JSON.stringify(app.session.commands.get())).not.toContain(typed);
});

it("shows no failure from a try the computer was unlocked and locked again during", async () => {
  const host = window.__sim?.host;
  if (!host) throw new Error("Missing host");
  host.unlockCheckMs = 1_000;
  render(<Desktop route={route} />, { wrapper });
  await advance(150);
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  fireEvent.click(screen.getByRole("button", { name: "Unlock with password" }));
  const field = () => screen.getByLabelText(/password$/) as HTMLInputElement;
  fireEvent.change(field(), { target: { value: "not it" } });
  fireEvent.submit(field().closest("form")!);
  await advance(150);
  act(() => window.__sim?.control.lock(false));
  await advance(150);
  act(() => window.__sim?.control.lock(true));
  await advance(150);
  expect(screen.getByText("Picture paused while locked")).toBeTruthy();
  await advance(1_000);
  const [sent] = unlocksSent();
  expect(app.session.state.get().commandStages[sent!.id]?.error?.message).toBe("That isn’t the computer’s password");
  expect(screen.queryByText("That isn’t the computer’s password")).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(unlocksSent()).toHaveLength(1);
});
