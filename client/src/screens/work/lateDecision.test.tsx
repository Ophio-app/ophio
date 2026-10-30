// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { AttentionList } from "./shared";

let app: App;
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0];
  if (!profile) throw new Error("Missing fixture profile");
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

function openRequest() {
  const route = { screen: "work" } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><AttentionList projectId={ids.pTide} /></RouteProvider></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: /Fix the flaky retry test.*Approval needed/ }));
}
const cancelled = () => ({ ...app.session.state.get().approvals[ids.aTest]!, state: "cancelled" as const, revision: 9 });

it("says an Allow the computer received after the run ended was not applied, with no success mark", async () => {
  openRequest();
  // The run ends on the computer before the phone hears of it.
  const host = window.__sim!.host;
  host.approvals = host.approvals.map(a => a.approval_id === ids.aTest ? { ...a, state: "cancelled" } : a);
  fireEvent.click(screen.getAllByRole("button", { name: "Allow" }).at(-1)!);
  await advance();
  act(() => host.emit({ type: "approval_upserted", approval: cancelled() }));
  expect(screen.getByText("No longer needed")).toBeTruthy();
  const line = screen.getByText("Arrived after the run ended; not applied").closest(".stateline")!;
  expect(line.classList.contains("stateline--neutral")).toBe(true);
  expect(line.previousElementSibling?.textContent).toBe("Allow");
  expect(screen.queryByText(/Accepted by/)).toBeNull();
  expect(screen.queryByText("Refused")).toBeNull();
});

it("does not keep Accepted beside a request whose run ended before the decision was recorded", async () => {
  openRequest();
  // An earlier computer admitted the decision, then the run's end cancelled the
  // request: the agent may have taken it, so nothing says it was not applied.
  fireEvent.click(screen.getAllByRole("button", { name: "Allow" }).at(-1)!);
  await advance();
  act(() => window.__sim!.host.emit({ type: "approval_upserted", approval: { ...cancelled(), delivery: undefined, resolved_by: null } }));
  const line = screen.getByText("Outcome unknown").closest(".stateline")!;
  expect(line.textContent).toContain("The run ended as this arrived; check what the agent did");
  expect(screen.queryByText("Arrived after the run ended; not applied")).toBeNull();
  expect(screen.queryByText(/Accepted by/)).toBeNull();
});

/** Allow admitted, then the computer records what became of it once the run's end overtook it. */
async function overtaken(stage: "failed" | "reconciliation_required", error: { code: "expired" | "outcome_unknown"; message: string }) {
  openRequest();
  fireEvent.click(screen.getAllByRole("button", { name: "Allow" }).at(-1)!);
  await advance();
  const command = Object.values(app.session.commands.get()).find(c => c.body.command === "resolve_approval")!;
  const host = window.__sim!.host;
  act(() => {
    host.emit({ type: "approval_upserted", approval: cancelled() });
    host.emit({ type: "command_progress", command_id: command.id, stage, detail: error.message, error });
  });
}

it("says a decision the computer could not hand on before the run ended was not applied", async () => {
  await overtaken("failed", { code: "expired", message: "Arrived after the run ended; not applied" });
  expect(screen.getByText("Arrived after the run ended; not applied").closest(".stateline")!.classList.contains("stateline--neutral")).toBe(true);
  expect(screen.queryByText("Outcome unknown")).toBeNull();
  expect(screen.queryByText(/Accepted by/)).toBeNull();
});

it("says a decision the agent took as its run ended may have been applied, not that it was not", async () => {
  await overtaken("reconciliation_required", { code: "outcome_unknown", message: "Reached the agent as its run ended; check what it did" });
  const line = screen.getByText("Outcome unknown").closest(".stateline")!;
  expect(line.classList.contains("stateline--warn")).toBe(true);
  expect(line.textContent).toContain("Reached the agent as its run ended; check what it did");
  expect(screen.queryByText("Arrived after the run ended; not applied")).toBeNull();
  expect(screen.queryByText(/Accepted by/)).toBeNull();
});
