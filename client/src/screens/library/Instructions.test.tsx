// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { InstructionSource } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import Instructions from "./Instructions";

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
const route = { screen: "instructions", taskId: ids.tFlaky } as const;
const headings = () =>
  screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent);
const source = (
  label: string,
  state: InstructionSource["state"],
  loaded_reported = false,
): InstructionSource => ({
  label,
  root_id: null,
  path: null,
  host_path: `/home/sam/Projects/tidewater-api/${label}`,
  harness: "claude_code",
  scope: "directory",
  state,
  loaded_reported,
  revision_at_start: null,
  current_revision: state === "missing" || state === "unknown" ? null : "abc",
});
beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  // Deterministic fixture revisions avoid native crypto worker scheduling under fake time.
  vi.spyOn(crypto.subtle, "digest").mockImplementation(async () =>
    new Uint8Array(32).buffer,
  );
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

it("lists the files it found and folds the places with no file into one line", async () => {
  render(<Instructions route={route} />, { wrapper });
  await advance(1000);
  expect(headings()).toEqual([
    "docs/guides/release.md",
    "CLAUDE.md",
    "docs/retry-policy.md",
    "Project settings",
  ]);
  expect(screen.getAllByRole("button", { name: "Open in Library" })).toHaveLength(3);
  const summary = screen.getByText("12 other places checked; none found");
  const details = summary.closest("details");
  expect(details?.open).toBe(false);
  expect(screen.queryByText("Not found")).toBeNull();
  fireEvent.click(summary);
  expect(details?.open).toBe(true);
  expect(screen.getAllByRole("listitem").map((li) => li.textContent)).toContain(
    "/home/sam/CLAUDE.local.md",
  );
});

it("keeps sources it couldn't inspect visible after the found ones", async () => {
  vi.spyOn(app.session, "query").mockResolvedValue({
    kind: "instructions",
    sources: [
      source("missing.md", "missing"),
      source("uninspected.md", "unknown"),
      source("discovered.md", "discovered"),
      source("changed.md", "changed"),
      source("reported.md", "discovered", true),
    ],
  });
  render(<Instructions route={route} />, { wrapper });
  await advance(1000);
  expect(headings()).toEqual([
    "reported.md",
    "discovered.md",
    "changed.md",
    "uninspected.md",
  ]);
  expect(
    screen.getByText("The computer couldn’t inspect this source"),
  ).toBeTruthy();
  expect(screen.getByText("1 other place checked; none found")).toBeTruthy();
});

it("drops “other” when nothing was found", async () => {
  vi.spyOn(app.session, "query").mockResolvedValue({
    kind: "instructions",
    sources: [source("a.md", "missing"), source("b.md", "missing")],
  });
  render(<Instructions route={route} />, { wrapper });
  await advance(1000);
  expect(screen.queryAllByRole("heading", { level: 2 })).toHaveLength(0);
  expect(screen.getByText("2 places checked; none found")).toBeTruthy();
});
