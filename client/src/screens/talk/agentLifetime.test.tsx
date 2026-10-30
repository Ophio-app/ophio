// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import Review from "./Review";

let app: App;
const tick = async () => act(async () => { await vi.advanceTimersByTimeAsync(900); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick();
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
function review(projectId: string) {
  app.ui.selectTask(projectId, null);
  app.ui.saveDraft(projectId, null, { text: "Check the layout" });
  const route = { screen: "review", projectId, taskId: null, intent: "new_task" } as const;
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}><Review route={route} /></RouteProvider></AppProvider>);
}

it.each([false, true])("a queued task keeps its own agent; new tasks honor only saved defaults (%s)", async remember => {
  const mounted = review(ids.pTide);
  await tick();
  fireEvent.click(screen.getByRole("button", { name: /Claude Code · Opus/ }));
  fireEvent.click(screen.getByRole("button", { name: /^Codex/ }));
  fireEvent.click(screen.getByRole("button", { name: "Medium" }));
  if (remember) fireEvent.click(screen.getByRole("button", { name: "Also use this agent, model and effort for new tasks in tidewater-api" }));
  fireEvent.click(screen.getByRole("button", { name: "Use this agent" }));
  await tick();
  fireEvent.click(screen.getByRole("button", { name: "Add to Next up" }));
  await tick();
  expect(app.ui.draft(ids.pTide, null)).toBeNull();
  const queued = Object.values(app.session.state.get().tasks).find(t => t.title === "Check the layout");
  expect(queued).toMatchObject({ harness: "codex", effort: "medium", state: "accepted" });
  mounted.unmount();
  const next = review(ids.pTide);
  await tick();
  expect(screen.getByRole("button", { name: remember ? /Codex ·/ : /Claude Code ·/ })).toBeTruthy();
  next.unmount();
  review(ids.pGarden);
  await tick();
  expect(screen.getByRole("button", { name: /Claude Code ·/ })).toBeTruthy();
});
