// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { applyTimelinePage } from "../../state/projection";
import File from "../library/File";
import { Transcript } from "./Transcript";

let app: App;
const advance = async (ms = 500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => (
  <AppProvider app={app}>
    <RouteProvider value={{ tab: "talk", route: { screen: "talk" }, depth: 1, active: true }}>{children}</RouteProvider>
  </AppProvider>
);
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(crypto.subtle, "digest").mockImplementation(async () => new Uint8Array(32).buffer);
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await advance(900);
});
afterEach(async () => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  await advance(20); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

it.each([
  ["/home/sam/Projects/tidewater-api", "/home/sam/Projects/tidewater-api/docs/retry-policy.md:12"],
  [String.raw`C:\Users\sam\site`, "C:/Users/sam/site/docs/retry-policy.md"],
  [String.raw`C:\Users\sam\site`, String.raw`C:\Users\sam\site\docs\retry-policy.md`],
  [String.raw`C:\Users\sam\site`, "C:/Users/sam/site/docs/retry-policy.md:12:5"],
  [String.raw`C:\Users\sam\site`, String.raw`C:\Users\sam\site\docs\retry-policy.md:12-20`],
  [String.raw`C:\Users\sam\site`, String.raw`C:\Users\sam\site\docs\.config.md`, "docs/.config.md"],
  [String.raw`C:\Users\sam\site`, String.raw`C:\Users\sam\site\docs\.config.md:12`, "docs/.config.md"],
  [String.raw`C:\Users\sam\site`, String.raw`C:\Users\sam\site\docs\_report.md`, "docs/_report.md"],
  [String.raw`C:\Users\sam\site`, String.raw`C:\Users\sam\site\docs\_report.md:12:5`, "docs/_report.md"],
  [String.raw`C:\_work`, String.raw`C:\_work\.config.md:12`, ".config.md"],
])("reads the Markdown file an agent links in %s at %s", async (hostPath, href, path = "docs/retry-policy.md") => {
  const { session } = app;
  vi.spyOn(session, "loadTimeline").mockResolvedValue();
  const files = window.__sim!.host.files[ids.rootInstr]!;
  files[path] = files["docs/retry-policy.md"]!;
  session.state.set((s) => ({ ...s, libraryRoots: s.libraryRoots.map((r) => r.root_id === ids.rootInstr ? { ...r, host_path: hostPath } : r) }));
  const task = session.state.get().tasks[ids.tHeaders]!;
  const text = `The retry rules are in [the policy](${href}).`;
  session.state.set((s) => applyTimelinePage(s, task.task_id, [
    { seq: 1, at: 1000, run_id: task.current_run!, item: { kind: "agent_message", text, partial: false } },
  ], false));
  const { unmount } = render(<Transcript task={task} agent="Codex" />, { wrapper });
  await advance(0);
  fireEvent.click(screen.getByRole("link", { name: "the policy" }));
  const route = app.ui.get().stacks.talk.at(-1);
  expect(route).toEqual({ screen: "file", rootId: ids.rootInstr, path, taskId: ids.tHeaders });
  unmount();

  const query = vi.spyOn(session, "query");
  render(<File route={route as Extract<typeof route, { screen: "file" }>} />, { wrapper });
  await advance(1000);
  expect(query).toHaveBeenCalledWith({ kind: "library_read", root_id: ids.rootInstr, path });
  expect(screen.getByRole("heading", { name: "Retry policy" })).toBeTruthy();
});
