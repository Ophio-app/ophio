// SPDX-License-Identifier: Apache-2.0
// "Tell me when it's done": a task's bell shows on the tap, reaches the
// computer as set_task_notify, and goes back with a toast when refused.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, useHost, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { Toasts } from "../../design/Toast";
import { CommandError, HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { TaskBell } from "./TaskBell";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});
function Bell() {
  const task = useHost((s) => s.tasks[ids.tHeaders]);
  return task ? <TaskBell task={task} /> : null;
}
const show = () => render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", route: { screen: "talk" }, depth: 0, active: true }}><Bell /><Toasts /></RouteProvider></AppProvider>);
const bell = () => screen.getByRole("button", { name: "Tell me when it’s done" });
const kept = () => window.__sim!.host.tasks.find((t) => t.task_id === ids.tHeaders)?.notify_done;

it("turns the bell on at the tap, sends set_task_notify, and shows the computer's answer", async () => {
  show();
  expect(bell().getAttribute("aria-pressed")).toBe("false");
  fireEvent.click(bell());
  expect(bell().getAttribute("aria-pressed")).toBe("true");
  expect(Object.values(app.session.commands.get()).at(-1)?.body).toEqual({ command: "set_task_notify", task_id: ids.tHeaders, on: true });
  await advance(900);
  expect(kept()).toBe(true);
  expect(app.session.state.get().tasks[ids.tHeaders]?.notify_done).toBe(true);
  expect(bell().getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(bell()); await advance(900);
  expect(kept()).toBeFalsy();
  expect(bell().getAttribute("aria-pressed")).toBe("false");
});

it("goes back with a toast when the computer refuses", async () => {
  show();
  vi.spyOn(app.session, "command").mockRejectedValueOnce(new CommandError({ code: "permission_denied", message: "Not allowed" }, null));
  fireEvent.click(bell());
  expect(bell().getAttribute("aria-pressed")).toBe("true");
  await advance(0);
  expect(bell().getAttribute("aria-pressed")).toBe("false");
  expect(screen.getByText(/^Couldn’t change whether this task tells you when it’s done\./)).toBeTruthy();
});

it("isn't offered by a computer that doesn't keep it", async () => {
  const caps = app.session.state.get().capabilities!;
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...caps, features: caps.features.filter((f) => f !== "alert_types_v1") } })));
  show();
  expect(screen.queryByRole("button", { name: "Tell me when it’s done" })).toBeNull();
});

it("is the person's: offered to this device, and refused by the computer without the tasks grant", async () => {
  show();
  // The phone is the person, so the bell is there to turn.
  expect(bell().hasAttribute("disabled")).toBe(false);
  // Without the tasks grant the computer refuses it, as a device may not.
  act(() => window.__sim!.permissions({ tasks: false })); await advance(900);
  let refused: unknown = "pending";
  void app.session.command({ command: "set_task_notify", task_id: ids.tHeaders, on: true }, { label: "Tell me when it’s done" }).then(() => { refused = null; }, (e: unknown) => { refused = e; });
  await advance(900);
  expect(refused).toBeInstanceOf(CommandError);
  expect(kept()).toBeFalsy();
});
