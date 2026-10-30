// SPDX-License-Identifier: Apache-2.0
import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../app/context";
import { UiStore } from "../app/ui";
import { createSimPlatform } from "../dev/sim/platform";
import { HostSession } from "../host/session";
import { StatusSheet, availabilityStatus, plainAvailability, usageLimitNow } from "./StatusSheet";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}>{children}</AppProvider>;
const row = (text: string | RegExp) => screen.getByText(text).closest(".evidence") as HTMLElement;

beforeEach(async () => {
  vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  localStorage.clear();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => undefined };
  session.start();
  await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers();
});

it("says an agent tool is out of usage, with its reset time, until that time passes", async () => {
  render(<StatusSheet open onClose={vi.fn()} />, { wrapper });
  act(() => window.__sim!.host.tablet.usageLimit("claude_code", Date.now() + 90_000));
  await advance();
  const claude = row("Claude Code");
  expect(claude.textContent).toMatch(/Usage limit reached\. Resets at .+\./);
  expect(claude.textContent).not.toContain("Ready to start tasks");
  expect(row("Codex").textContent).toContain("Ready to start tasks");
  await advance(90_000);
  expect(row("Claude Code").textContent).toContain("Ready to start tasks");
});

it("goes back to ready once the computer reports usage returned", async () => {
  render(<StatusSheet open onClose={vi.fn()} />, { wrapper });
  act(() => window.__sim!.host.tablet.usageLimit("codex", null));
  await advance();
  expect(row("Codex").textContent).toContain("Usage limit reached.");
  expect(row("Codex").textContent).not.toContain("Resets");
  const host = window.__sim!.host;
  act(() => host.emit({ type: "harnesses_changed", harnesses: host.harnesses.map(({ usage_limit: _, ...h }) => h) }));
  await advance();
  expect(row("Codex").textContent).toContain("Ready to start tasks");
});

it("words each capability check for the person and keeps the tool's reason on request", async () => {
  render(<StatusSheet open onClose={vi.fn()} />, { wrapper });
  fireEvent.click(screen.getByRole("button", { name: "Run checks" }));
  await advance(3000);
  const delegation = row("Pi can’t use the computer for you");
  const reason = "Pi runs only in a terminal";
  // Unsupported is not a check that failed or didn't run.
  expect(delegation.querySelector(".evidence__word")).toBeNull();
  expect(delegation.querySelector(".evidence__meta")?.textContent).not.toContain(reason);
  const details = delegation.querySelector("details");
  expect(details?.querySelector("summary")?.textContent).toBe("Details");
  expect(details?.textContent).toContain(reason);
  expect(row("Not known whether you can steer Claude Code while it works").textContent).toContain("Unknown");
  // Checks from a computer that doesn't word them keep their label and detail.
  expect(row("Codex sign-in").textContent).toContain("Personal · Pro.");
});

it("states what each availability means and keeps the reason apart", () => {
  expect(plainAvailability({ state: "unsupported", reason: "No portal" })).toEqual({ text: "Not available on this computer.", reason: "No portal" });
  expect(plainAvailability({ state: "temporarily_unavailable", reason: "Session locked" })).toEqual({ text: "Not right now.", reason: "Session locked" });
  // Out for now, as while the computer is locked, is not a failure.
  expect(availabilityStatus({ state: "temporarily_unavailable", reason: "Session locked" })).toBe("unavailable");
  expect(plainAvailability({ state: "permission_missing", remedy: "Run the setup" })).toEqual({ text: "Needs setup. Run the setup.", reason: null });
  expect(plainAvailability({ state: "available" })).toEqual({ text: null, reason: null });
  expect(usageLimitNow({ reached_at: 0, resets_at: 100 }, 99)).not.toBeNull();
  expect(usageLimitNow({ reached_at: 0, resets_at: 100 }, 100)).toBeNull();
  expect(usageLimitNow({ reached_at: 0, resets_at: null }, 1e15)).not.toBeNull();
  expect(usageLimitNow(undefined, 0)).toBeNull();
});

it("warns that another device answered, keeps trying, and returns to normal when the computer answers", async () => {
  const forget = vi.fn(async () => undefined);
  app = { ...app, forget };
  render(<StatusSheet open onClose={vi.fn()} />, { wrapper });
  act(() => window.__sim!.impostor());
  await advance(1000);
  expect(screen.getByText("Another device answered at studio’s address")).toBeTruthy();
  expect(screen.getByText(/so nothing was sent to it\. .* The phone keeps trying and reconnects by itself once studio answers\./)).toBeTruthy();
  expect(row("Computer identity").querySelector(".evidence__icon--failed")).toBeTruthy();
  expect(row("Computer identity").textContent).toContain("The device that answered didn’t have studio’s certificate");
  expect(row("Offline").textContent).toMatch(/Another device answered at the computer’s address.*Trying again/);
  // Trying again and forgetting are both there; neither is the only way on.
  expect(screen.getByRole("button", { name: "Try again now" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Forget studio and pair again" }));
  expect(forget).toHaveBeenCalledOnce();
  // Still another device, attempt after attempt.
  await advance(10_000);
  expect(screen.getByText("Another device answered at studio’s address")).toBeTruthy();
  act(() => window.__sim!.online());
  await advance(5000);
  expect(screen.queryByText("Another device answered at studio’s address")).toBeNull();
  // The fingerprint waits under Details, whole; the row says what it means.
  const identity = row("Computer identity verified");
  expect(identity.querySelector(".evidence__meta")!.textContent).toContain("studio showed the certificate it had when you paired");
  expect(identity.querySelector(".evidence__meta")!.textContent).not.toMatch(/[0-9A-F]{4} [0-9A-F]{4}/);
  expect(identity.querySelector(".evidence__reason")!.textContent).toMatch(/saved when you paired: ([0-9A-F]{4} ){15}[0-9A-F]{4}$/);
});
