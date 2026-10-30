// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import type { TimelineBody } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { Transcript } from "./Transcript";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

it("says a held message was withdrawn because its chat lost access, and offers no resend", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.state === "working" && t.harness === "claude_code")!;
  // As the computer records a chat's follow-up held for the next turn, then
  // its withdrawal when that chat is revoked.
  const append = (item: TimelineBody) => host.emit({ type: "timeline_appended", task_id: task.task_id,
    item: { seq: host.seq + 1, at: Date.now(), run_id: task.current_run, item } });
  const held = append({ kind: "user_message", text: "Sent from the group chat.", bundle: null, delivery: "queued_for_boundary" });
  await tick();
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: { screen: "talk" } }}>
    <Transcript task={app.session.state.get().tasks[task.task_id]!} agent="Claude Code" />
  </RouteProvider></AppProvider>);
  await tick();
  expect(screen.getByText("Held until this turn ends")).toBeTruthy();
  append({ kind: "delivery", message_seq: held, state: "cancelled", withdrawn_by_revocation: true });
  await tick();
  expect(screen.getByText("Withdrawn when its chat lost access · not sent")).toBeTruthy();
  expect(screen.queryByText("Held until this turn ends")).toBeNull();
  expect(screen.queryByRole("button", { name: "Put back in composer" })).toBeNull();
});

it("a Cancel whose outcome a restarted computer can't report does not lock the held message", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find(t => t.state === "working" && t.harness === "claude_code")!;
  host.emit({ type: "timeline_appended", task_id: task.task_id,
    item: { seq: host.seq + 1, at: Date.now(), run_id: task.current_run, item: { kind: "user_message", text: "After this step, run the tests.", bundle: null, delivery: "queued_for_boundary" } } });
  await tick();
  render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route: { screen: "talk" } }}>
    <Transcript task={app.session.state.get().tasks[task.task_id]!} agent="Claude Code" />
  </RouteProvider></AppProvider>);
  await tick();
  const cancel = () => screen.getByRole("button", { name: "Cancel" }) as HTMLButtonElement;
  expect(cancel().disabled).toBe(false);

  // The computer's service goes down while the Cancel is on its way.
  vi.spyOn(host, "command").mockImplementationOnce(() => new Promise(() => {}));
  fireEvent.click(cancel());
  await tick(100);
  await act(async () => window.__sim!.offline());
  await tick(600);
  // While the outcome can still be learned, Cancel waits for it.
  expect(cancel().disabled).toBe(true);

  // It comes back as a new boot, which can't say what the old one received.
  (host as { bootEpoch: string }).bootEpoch = "0192f7a1-0000-7000-8000-00000000b003";
  await act(async () => window.__sim!.online());
  while (app.session.link.get().state !== "online") await tick(100);
  await tick(3000);
  expect(Object.values(app.session.commands.get()).find((c) => c.body.command === "cancel_message")?.phase).toBe("unknown");
  expect(screen.getByText("Held until this turn ends")).toBeTruthy();
  expect(cancel().disabled).toBe(false);
});
