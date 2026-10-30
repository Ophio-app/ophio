// SPDX-License-Identifier: Apache-2.0
import { StrictMode, type ReactNode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { RouteProvider } from "../../shell/Screen";
import type { LeaseView } from "../../protocol/types";
import { useControl } from "./useControl";
import Desktop from "./Desktop";
import type { Picture } from "./useMedia";
import { dropVoice, holdVoice, startVoice, useVoiceHold, voiceHolds, voiceHoldsLease } from "./voiceHold";

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
const commands = () => Object.values(app.session.commands.get()).map((c) => c.body.command);
const target = () => {
  const t = app.session.state.get().targets.find((t) => t.target_id === ids.tgHdmi);
  if (!t) throw new Error("Missing target");
  return t;
};
const lease = (id: string): LeaseView => ({ lease_id: id, target_id: ids.tgHdmi, owner: { kind: "human", device_id: app.profile.deviceId }, generation: 1, boot_epoch: "boot", state: "active", granted_at: 1, renewal_window_ms: 3000 });

beforeEach(async () => {
  vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
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
  const sim = window.__sim?.host;
  if (!sim) throw new Error("Missing simulated host");
  sim.voice.desktopAvailable = true;
  sim.voice.link(sim);
  picture.current = { at: Date.now(), revision: target().geometry.revision, connected: true, error: null };
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
});

vi.mock("./useMedia", async (importOriginal) => ({
  ...await importOriginal<typeof import("./useMedia")>(),
  useMedia: () => ({ picture: picture.current, latest: picture, canvas: { current: null }, retry: vi.fn(), reopen: vi.fn() }),
}));

it("holds a display's voice lease from start to a matching drop, and restores it after a reload", () => {
  const { session, profile } = app;
  holdVoice(session, profile.profileId, ids.tgHdmi, { sessionId: null, leaseId: null });
  // Still starting: any lease of this device on the display is voice's.
  expect(voiceHoldsLease(session, lease("any"))).toBe(true);
  const saved = `voice-session.${profile.profileId}.${ids.tgHdmi}`;
  expect(localStorage.getItem(saved)).toBeNull();
  holdVoice(session, profile.profileId, ids.tgHdmi, { sessionId: "s1", leaseId: "l1" });
  expect(voiceHoldsLease(session, lease("l1"))).toBe(true);
  expect(voiceHoldsLease(session, lease("l2"))).toBe(false);
  dropVoice(session, profile.profileId, ids.tgHdmi, "other");
  expect(voiceHoldsLease(session, lease("l1"))).toBe(true);
  // A reloaded app has only what was saved.
  voiceHolds(session).set(() => ({}));
  const { result } = renderHook(() => useVoiceHold(session, profile.profileId, ids.tgHdmi), { wrapper });
  expect(result.current).toEqual({ sessionId: "s1", leaseId: "l1" });
  act(() => dropVoice(session, profile.profileId, ids.tgHdmi, "s1"));
  expect(result.current).toBeUndefined();
  expect(localStorage.getItem(saved)).toBeNull();
});

it("a view of the display neither takes touch input under voice control nor releases its lease", async () => {
  const { result, rerender, unmount } = renderHook(
    ({ visible }) => useControl(ids.tgHdmi, picture, visible),
    { wrapper, initialProps: { visible: true } },
  );
  await advance(100);
  let started: Awaited<ReturnType<typeof startVoice>> = null;
  await act(async () => {
    const starting = startVoice(app.session, app.profile.profileId, target(), "desktop", false);
    await vi.advanceTimersByTimeAsync(300);
    started = await starting;
  });
  expect(started).not.toBeNull();
  await advance(100);
  expect(result.current.voice).toBe(true);
  expect(result.current.mine).toBe(false);
  expect(result.current.send({ action: "text", text: "must not send" })).toBe(false);
  // Hidden, then gone: each used to release the lease and end voice control.
  rerender({ visible: false });
  await advance(200);
  unmount();
  await advance(200);
  expect(commands()).not.toContain("release_lease");
  expect(app.session.state.get().control[ids.tgHdmi]?.lease?.state).toBe("active");
});

it("starts voice control from the live desktop, keeps the picture watch-only and stops it there", async () => {
  render(<Desktop route={{ screen: "desktop", targetId: ids.tgHdmi }} />, { wrapper });
  await advance(300);
  fireEvent.click(screen.getByRole("button", { name: "Voice" }));
  await advance(300);
  expect(screen.getByText(/including touch input from this device/)).toBeTruthy();
  fireEvent.click(screen.getByLabelText("Preview only · send no input"));
  fireEvent.click(screen.getByRole("button", { name: "Accept and take over" }));
  await advance(1500);
  expect(commands()).toContain("voice");
  expect(screen.getByText("Voice control · touch off")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Stop voice" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Hold to talk" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Voice" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Keyboard" })).toBeNull();

  // A sensitive field is typed: Stop voice hands input back to this device.
  fireEvent.change(screen.getByLabelText("Typed command"), { target: { value: "type my password" } });
  fireEvent.click(screen.getByRole("button", { name: "Send command" }));
  await advance(1500);
  fireEvent.click(screen.getByRole("button", { name: "Stop voice and use keyboard" }));
  await advance(2000);
  expect(commands()).toContain("request_human_lease");
  expect(commands()).not.toContain("release_lease");
  expect(screen.getByText(/^This device holds input/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Leave control" })).toBeTruthy();
  expect(screen.queryByText("Voice control ended")).toBeNull();
});

it("keeps an ended session on screen until closed", async () => {
  render(<Desktop route={{ screen: "desktop", targetId: ids.tgHdmi }} />, { wrapper });
  await advance(300);
  fireEvent.click(screen.getByRole("button", { name: "Voice" }));
  await advance(300);
  fireEvent.click(screen.getByRole("button", { name: "Accept and preview commands" }));
  await advance(1500);
  expect(document.querySelector(".ownership__title")?.textContent).toBe("Voice preview");
  fireEvent.click(screen.getByRole("button", { name: "Stop voice" }));
  await advance(1500);
  expect(screen.getByText("Voice control ended")).toBeTruthy();
  // Ended, the display offers a new session and touch control again.
  expect(screen.getByRole("button", { name: "Voice" })).toBeTruthy();
  expect(screen.getByText("Watch · read-only")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  expect(screen.queryByText("Voice control ended")).toBeNull();
  expect(commands().filter((c) => c === "release_lease")).toEqual([]);
});
