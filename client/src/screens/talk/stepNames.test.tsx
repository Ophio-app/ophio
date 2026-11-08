// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { HostSession } from "../../host/session";
import { toolLabel } from "./toolInput";
import { Steps } from "./Transcript";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); });

it("names Codex's kinds of step in plain words, and any other tool as given", () => {
  expect(toolLabel("commandExecution")).toBe("Command");
  expect(toolLabel("fileChange")).toBe("File change");
  expect(toolLabel("webSearch")).toBe("Web search");
  expect(toolLabel("ExitPlanMode")).toBe("Plan");
  expect(toolLabel("Bash")).toBe("Bash");
  expect(toolLabel("show_person")).toBe("show_person");
});

it("an opened Codex step reads as a command, not Codex's name for it", async () => {
  const step = (seq: number, key: string, tool: string, summary: string, detail: string, status: "succeeded" | "failed") => ({
    seq, at: Date.UTC(2026, 10, 7, 13, 12) + seq, run_id: "run",
    item: { kind: "activity" as const, tool, summary, status, detail, key },
  });
  render(<AppProvider app={app}><Steps ended steps={[
    step(1, "a", "commandExecution", "Command finished", "ls", "succeeded"),
    step(2, "b", "commandExecution", "Command finished", "ls /Users/sam/Documents", "failed"),
  ]} /></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
  fireEvent.click(screen.getByRole("button", { expanded: false }));
  expect(screen.getAllByText(/^Command · /)).toHaveLength(2);
  expect(document.body.textContent).not.toContain("commandExecution");
});
