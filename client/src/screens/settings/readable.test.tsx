// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import { createSimPlatform } from "../../dev/sim/platform";
import { AgentsAndAccounts } from "./AgentsAndAccounts";
import { Notifications } from "./Notifications";

// What a phone screen reader reads for Settings controls.
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

it("names the notification choice by its label and says what it does as its description", async () => {
  render(<Notifications />, { wrapper });
  await advance(100);
  const choice = screen.getByRole("combobox", {
    name: "Notification detail",
    description: "Shows the computer and project, not the task’s title.",
  });
  // The label names the choice instead of wrapping it and its hint together.
  expect(choice.closest("label")).toBeNull();
});

it("tells each agent's models apart when they are listed together", async () => {
  render(<AgentsAndAccounts />, { wrapper });
  await advance(1000);
  const names = [...document.querySelectorAll("summary")]
    .map((s) => s.textContent)
    .filter((n) => n?.startsWith("Models, and what works from the phone"));
  expect(names.length).toBeGreaterThan(1);
  expect(new Set(names).size).toBe(names.length);
  expect(names).toContain("Models, and what works from the phone for Claude Code");
});

// A phone screen reader stops on each text node beside the age, so the line's
// words must be one node: "Model list checked", then "4 minutes ago".
it("reads an agent's catalog line as its words, then its age", async () => {
  render(<AgentsAndAccounts />, { wrapper });
  await advance(1000);
  const lines = [...document.querySelectorAll("p")].filter((p) => p.querySelector("time") && p.textContent?.includes("Model list checked"));
  expect(lines.length).toBeGreaterThan(1);
  for (const line of lines) expect([...line.childNodes].map((n) => n.nodeName)).toEqual(["#text", "TIME"]);
});
