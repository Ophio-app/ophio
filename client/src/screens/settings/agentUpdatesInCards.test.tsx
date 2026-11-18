// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { AgentsAndAccounts } from "./AgentsAndAccounts";

// Agent apps: each agent's version is said once, with its update line in its own card.
let app: App;
const advance = (ms: number) =>
  act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}>{children}</AppProvider>;

beforeEach(async () => {
  vi.useFakeTimers();
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
});

const card = (name: string) => screen.getByRole("heading", { name }).closest("article")!;

it("gives each agent's update line in its own card, below the version it already shows", async () => {
  render(<AgentsAndAccounts />, { wrapper });
  await advance(1000);
  const updates = screen.getByRole("article", { name: "Agent updates" });
  // The setting and Check now stay together; the versions are not repeated there.
  expect(within(updates).getAllByRole("radio")).toHaveLength(3);
  expect(within(updates).getByRole("button", { name: "Check now" })).toBeTruthy();
  expect(updates.textContent).not.toMatch(/2\.4\.1|0\.212\.0/);
  const claude = card("Claude Code");
  expect(claude.textContent?.match(/2\.4\.1/g)).toHaveLength(1);
  expect(within(claude).getByText("Updates: 2.4.2 is out")).toBeTruthy();
  const codex = card("Codex");
  expect(within(codex).getByText("Updates: latest version unknown")).toBeTruthy();
  expect(within(codex).getByText(/reach the update site/)).toBeTruthy();
  expect(within(codex).queryByRole("button", { name: /^Update .* now$/ })).toBeNull();
  // An agent the app can't use has no update line.
  expect(card("Hermes").textContent).not.toMatch(/Updates:/);
});

it("updates an agent from its own card and says so there", async () => {
  render(<AgentsAndAccounts />, { wrapper });
  await advance(1000);
  fireEvent.click(within(card("Claude Code")).getByRole("button", { name: "Update Claude Code now" }));
  await advance(1000);
  const claude = card("Claude Code");
  expect(within(claude).getByText("Update requested. Your update setting stays the same.")).toBeTruthy();
  expect(within(claude).getByText("Updates: up to date")).toBeTruthy();
  expect(claude.textContent?.match(/2\.4\.2/g)).toHaveLength(1);
  expect(screen.getByRole("article", { name: "Agent updates" }).textContent).not.toMatch(/Update requested/);
});

it("names the version tasks start only when it differs from the card's own", async () => {
  const sim = window.__sim!.host.agentUpdates;
  sim.view = { ...sim.view, agents: sim.view.agents.map((a) => a.harness === "claude_code" ? { ...a, installed_version: "2.4.0" } : a) };
  render(<AgentsAndAccounts />, { wrapper });
  await advance(1000);
  expect(within(card("Claude Code")).getByText("Tasks use 2.4.0 · Updates: 2.4.2 is out")).toBeTruthy();
  expect(within(card("Codex")).getByText("Updates: latest version unknown")).toBeTruthy();
});
