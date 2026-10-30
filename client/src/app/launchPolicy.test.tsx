// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "./context";
import { UiStore } from "./ui";
import { prefs } from "./prefs";
import { defaultLaunchPolicy, launchChoices, launchFlags, launchRisk, launchSummary } from "./launchPolicy";
import { HostSession } from "../host/session";
import { createSimPlatform } from "../dev/sim/platform";
import { LaunchDefaults } from "../screens/settings/LaunchDefaults";
import { resolveAgent, agentChoiceBlocker } from "../screens/talk/intent";
import { harnesses, projects } from "../dev/sim/scenario";

let app: App;
const advance = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}>{children}</AppProvider>;
beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.clear();
  prefs.set(p => ({ ...p, claudeBypassPermissions: true, codexBypassPermissions: true, defaultAgent: null }));
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing simulated profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
});
afterEach(async () => {
  cleanup();
  await act(async () => { await Promise.resolve(); });
  app.session.stop();
  window.__sim?.host.dispose();
  vi.clearAllTimers();
  vi.useRealTimers();
  prefs.set(p => ({ ...p, defaultAgent: null }));
});
it("keeps one default agent for new work, and says when it isn't ready", async () => {
  render(<LaunchDefaults />, { wrapper });
  await advance(100);
  const group = within(screen.getByRole("radiogroup", { name: "Default agent" }));
  expect(group.getByRole("radio", { name: /First ready agent/ }).getAttribute("aria-checked")).toBe("true");
  // Each option carries a mark, not only a tint, so the chosen one is plain.
  expect(group.getAllByRole("radio").every((r) => r.querySelector(".row__radio"))).toBe(true);
  fireEvent.click(group.getByRole("radio", { name: /^Codex/ }));
  expect(prefs.get().defaultAgent).toBe("codex");
  expect(JSON.parse(localStorage.getItem("prefs")!)).toMatchObject({ defaultAgent: "codex" });
  expect(resolveAgent({ ...projects(1000)[0]!, default_harness: null }, harnesses, null)?.harness).toBe("codex");
  expect(screen.queryByText(/isn’t ready on/)).toBeNull();
  // A default saved on another computer stays chosen where it can't run, and says so.
  act(() => prefs.set(p => ({ ...p, defaultAgent: "pi" })));
  expect(group.getByRole("radio", { name: /^Pi/ }).getAttribute("aria-checked")).toBe("true");
  expect(screen.getByText(/Pi isn’t ready on .*, so new work there starts with the first ready agent\./)).toBeTruthy();
});
it("offers independent persistent defaults and preserves task overrides", () => {
  render(<LaunchDefaults />, { wrapper });
  const claude = screen.getByRole<HTMLInputElement>("checkbox", { name: "Claude Code: don’t ask first" });
  const codex = screen.getByRole<HTMLInputElement>("checkbox", { name: "Codex: don’t ask first, and turn off its command sandbox" });
  expect(claude.checked).toBe(true); expect(codex.checked).toBe(true);
  fireEvent.click(claude);
  expect(defaultLaunchPolicy("claude_code")).toBe("standard");
  expect(defaultLaunchPolicy("codex")).toBe("bypass_permissions");
  expect(codex.checked).toBe(true);
  fireEvent.click(codex);
  expect(JSON.parse(localStorage.getItem("prefs")!)).toMatchObject({ claudeBypassPermissions: false, codexBypassPermissions: false });
  const override = { harness: "codex" as const, model: null, effort: null, launchPolicy: "sandboxed" as const };
  expect(resolveAgent(projects(1000)[0], harnesses, override)).toEqual(override);
  expect(resolveAgent(projects(1000)[0], harnesses, { ...override, launchPolicy: null })?.launchPolicy).toBeNull();
  expect(resolveAgent(projects(1000)[0], harnesses, { ...override, launchPolicy: undefined })?.launchPolicy).toBe("standard");
});
it("offers only supported typed policies and never describes read-only modes as bypassed", () => {
  expect(launchChoices("claude_code").map(c => c.value)).toEqual(["bypass_permissions", "standard", "accept_edits"]);
  expect(launchChoices("codex").map(c => c.value)).toEqual(["bypass_permissions", "standard", "sandboxed"]);
  expect(launchChoices("pi")).toEqual([]);
  expect(defaultLaunchPolicy("pi")).toBeNull();
  expect(launchSummary("codex", "bypass_permissions", "plan")).toContain("Read-only");
  // Flags are for those who look; skipping prompts is said in words, in Work only.
  expect(launchSummary("claude_code", "bypass_permissions")).toBe("Skip permissions");
  expect(launchFlags("claude_code", "bypass_permissions")).toBe("--dangerously-skip-permissions");
  expect(launchFlags("codex", "bypass_permissions", "plan")).toBeNull();
  expect(launchRisk("codex", "bypass_permissions")).toMatch(/outside its command sandbox/);
  expect(launchRisk("claude_code", "standard")).toBeNull();
  expect(launchRisk("claude_code", "bypass_permissions", "ask")).toBeNull();
  expect(agentChoiceBlocker(harnesses.find(h => h.kind === "codex"), { harness: "codex", model: null, effort: null, launchPolicy: "accept_edits" })).toContain("permission setting is unavailable");
});
