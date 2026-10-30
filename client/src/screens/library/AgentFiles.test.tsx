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
import AgentFiles from "./AgentFiles";
import Library from "./Library";

let app: App;
const advance = async (ms = 500) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
const wrapper = ({ children }: { children: ReactNode }) => (
  <AppProvider app={app}>
    <RouteProvider
      value={{
        tab: "library",
        route: { screen: "library" },
        depth: 1,
        active: true,
      }}
    >
      {children}
    </RouteProvider>
  </AppProvider>
);
const section = (title: string) =>
  screen.getByRole("heading", { level: 2, name: title }).closest("section")!;
const rows = (within: Element) =>
  [...within.querySelectorAll(".row__title")].map((t) => t.textContent);
beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform(),
    profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing profile");
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
});
afterEach(async () => {
  cleanup();
  app.session.stop();
  window.__sim?.host.dispose();
  app.ui.persistNow();
  await advance(20);
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

it("lists agent files by kind, apart from the folders on the computer", async () => {
  render(<Library route={{ screen: "library" }} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("radio", { name: "Files" }));
  await advance(1000);
  expect(rows(section("Agent files"))).toEqual([
    "Instructions",
    "Agents",
    "Skills",
    "Commands",
    "Memory",
  ]);
  // Its counts are files, apart from the skills Library lists.
  expect(within(section("Agent files")).getByRole("button", { name: /^Skills/ }).textContent).toMatch(/\d+ files?$/);
  // The folder that only reaches agent files isn't one of them.
  expect(rows(section("Folders on the computer"))).not.toContain("Claude Code");
  fireEvent.click(
    within(section("Agent files")).getByRole("button", { name: /^Skills/ }),
  );
  expect(app.ui.route()).toEqual({ screen: "agent_files", group: "skills" });
});

it("groups one kind by where it applies and opens each file", async () => {
  render(<AgentFiles route={{ screen: "agent_files", group: "instructions" }} />, { wrapper });
  await advance(1000);
  expect(rows(section("Claude Code"))).toEqual(["CLAUDE.md"]);
  expect(rows(section("tidewater-api"))).toEqual(["AGENTS.md", "CLAUDE.md"]);
  fireEvent.click(screen.getAllByRole("button", { name: /CLAUDE\.md/ })[0]!);
  expect(app.ui.route()).toEqual({
    screen: "file",
    rootId: ids.rootClaude,
    path: "CLAUDE.md",
  });
});

it("names memory by the project it is kept for", async () => {
  render(<AgentFiles route={{ screen: "agent_files", group: "memory" }} />, { wrapper });
  await advance(1000);
  expect(rows(section("tidewater-api"))).toEqual(["MEMORY.md", "payments.md"]);
});

it("narrows Library's files to the chosen project's, and Home has them all", async () => {
  app.ui.selectProject(ids.pTide);
  const { unmount } = render(<Library route={{ screen: "library" }} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("radio", { name: "Files" }));
  await advance(1000);
  expect(rows(section("Folders on the computer"))).toEqual(["tidewater-api"]);
  expect(rows(section("Agent files"))).not.toContain("Skills");
  unmount();
  render(<AgentFiles route={{ screen: "agent_files", group: "instructions" }} />, { wrapper });
  await advance(1000);
  expect(rows(section("tidewater-api"))).toEqual(["AGENTS.md", "CLAUDE.md"]);
  expect(screen.queryByRole("heading", { level: 2, name: "Claude Code" })).toBeNull();
  cleanup();

  act(() => app.ui.selectProject(ids.pHome));
  render(<Library route={{ screen: "library" }} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("radio", { name: "Files" }));
  await advance(1000);
  expect(rows(section("Folders on the computer"))).toContain("tidewater-api");
  expect(rows(section("Folders on the computer")).length).toBeGreaterThan(1);
});

it("finds a file by its name, though its words never say it, and opens it", async () => {
  render(<Library route={{ screen: "library" }} />, { wrapper });
  await advance(1000);
  fireEvent.click(screen.getByRole("radio", { name: "Files" }));
  await advance(1000);
  fireEvent.change(screen.getByRole("searchbox", { name: "Search files" }), {
    target: { value: "Retry-Policy" },
  });
  await advance(1000);
  const found = within(section("Search results")).getAllByRole("button", {
    name: /retry-policy\.md/,
  });
  expect(found).toHaveLength(1);
  // A name match shows no line and no words from inside the file.
  expect(found[0]!.textContent).not.toMatch(/line|Exponential/);
  fireEvent.click(found[0]!);
  expect(app.ui.route()).toEqual({
    screen: "file",
    rootId: ids.rootInstr,
    path: "docs/retry-policy.md",
  });
});
