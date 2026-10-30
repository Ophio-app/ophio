// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession, NotConnectedError } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { AgentSheet } from "./AgentSheet";
import Talk from "./Talk";
import Receipt from "../work/Receipt";
import { workIds } from "../../dev/sim/work";
import { Transcript } from "./Transcript";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
function view(child: React.ReactNode, route: Route = { screen: "talk" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{child}</RouteProvider></AppProvider>);
}
it("retries conversation history on link-up and clears the failed fetch", async () => {
  const task = app.session.state.get().tasks[ids.tDash]!;
  const load = vi.spyOn(app.session, "loadTimeline").mockRejectedValueOnce(new NotConnectedError());
  view(<Transcript task={task} agent="Claude Code" />);
  await tick();
  expect(screen.getByText(/Couldn’t load/)).toBeTruthy();
  act(() => window.__sim!.offline());
  act(() => window.__sim!.online());
  await tick(900);
  expect(load.mock.calls.length).toBeGreaterThan(1);
  expect(screen.queryByText(/Couldn’t load/)).toBeNull();
  expect(screen.getByRole("list", { name: "Conversation" })).toBeTruthy();
});
it("ignores a late failure from a previous task", async () => {
  let fail: (e: Error) => void = () => {};
  vi.spyOn(app.session, "loadTimeline").mockImplementationOnce(() => new Promise((_, reject) => { fail = reject; }));
  const state = app.session.state.get();
  const first = state.tasks[ids.tDash]!;
  const next = state.tasks[ids.tHeaders]!;
  const v = view(<Transcript task={first} agent="Claude Code" />);
  v.rerender(<AppProvider app={app}><Transcript task={next} agent="Codex" /></AppProvider>);
  await tick();
  await act(async () => fail(new Error("Old task failure")));
  expect(screen.queryByText("Old task failure")).toBeNull();
});

it("renders display names in Chat, the agent sheet and receipts", async () => {
  const assertNames = () => expect(document.body.textContent).not.toMatch(/claude-opus-5-5|gpt-6-astra/);
  const t = view(<Talk route={{ screen: "talk" }} />); await tick(); assertNames(); t.unmount();
  const receipt = view(<Receipt route={{ screen: "receipt", taskId: workIds.receipt }} />); await tick(); assertNames(); receipt.unmount();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const project = app.session.state.get().projects[ids.pTide]!;
  view(<AgentSheet open onClose={() => {}} project={project} value={{ harness: "claude_code", model: "claude-opus-5-5", effort: "high" }} onChange={() => {}} />);
  await tick(); assertNames();
});
// A phone screen reader reads each text node beside a button as its own stop,
// so a run's end would be heard as "Completed", "·" and the time apart.
it("keeps each run divider's words in one piece beside its What happened link", async () => {
  view(<Transcript task={app.session.state.get().tasks[ids.tHeaders]!} agent="Claude Code" />);
  await tick();
  const ended = screen.getByRole("button", { name: "What happened" }).parentElement!;
  const words = ended.querySelector(":scope > span")!;
  expect(words.childNodes).toHaveLength(1);
  // A status, then when it ended; the date shows too once that was another day.
  expect(words.textContent).toMatch(/^\S.* · .*\d:\d\d/);
  for (const divider of document.querySelectorAll(".talk-divider > span")) expect(divider.childNodes).toHaveLength(1);
});
