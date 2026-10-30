// SPDX-License-Identifier: Apache-2.0
// Actions wait while a related command's outcome can still be learned. When
// the computer restarts before it can say what happened, the phone never
// will: the actions must come back, and the note must not ask for a
// reconnect that cannot help.
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import type { Command } from "../../protocol/types";
import { CommandFeedback, useUnconfirmedCommand } from "./shared";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

function TaskActions({ taskId }: { taskId: string }) {
  const match = (c: Command) => "task_id" in c && c.task_id === taskId;
  const uncertain = useUnconfirmedCommand(match);
  return <><p>{uncertain ? "Waiting for the computer" : "Actions available"}</p><CommandFeedback match={match} /></>;
}

it("a task command lost to a computer restart does not lock the task's actions for good", async () => {
  const host = window.__sim!.host;
  const task = host.tasks[0]!;
  render(<AppProvider app={app}><TaskActions taskId={task.task_id} /></AppProvider>);
  expect(screen.getByText("Actions available")).toBeTruthy();

  // The computer's service goes down while a rename is on its way.
  vi.spyOn(host, "command").mockImplementationOnce(() => new Promise(() => {}));
  void app.session.command({ command: "rename_task", task_id: task.task_id, title: "Renamed" }, { label: "Rename task", expectedRevision: task.revision }).catch(() => undefined);
  await tick(100);
  await act(async () => window.__sim!.offline());
  await tick(600);
  // While the outcome can still be learned, the actions wait for it.
  expect(screen.getByText("Waiting for the computer")).toBeTruthy();

  // It comes back as a new boot, which can't say what the old one received.
  (host as { bootEpoch: string }).bootEpoch = "0192f7a1-0000-7000-8000-00000000b002";
  await act(async () => window.__sim!.online());
  while (app.session.link.get().state !== "online") await tick(100);
  await tick(3000);
  expect(Object.values(app.session.commands.get()).find((c) => c.body.command === "rename_task")?.phase).toBe("unknown");
  await tick(120_000);
  expect(screen.getByText("Actions available")).toBeTruthy();
  expect(screen.queryByText(/Reconnect to reconcile it/)).toBeNull();
  expect(screen.getByText(/restarted, so it can’t say whether this happened/)).toBeTruthy();
});
