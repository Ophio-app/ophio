// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { ids } from "../../dev/sim/scenario";
import { createSimPlatform } from "../../dev/sim/platform";
import { workIds } from "../../dev/sim/work";
import { HostSession } from "../../host/session";
import type { ActivityStatus, Run } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import type { Step } from "../talk/timeline";
import { observedWork } from "./observed";
import Receipt from "./Receipt";

let seq = 0;
const step = (tool: string, detail: string | null, status: ActivityStatus = "succeeded", summary = tool): Step =>
  ({ seq: ++seq, at: 0, run_id: "r", item: { kind: "activity", tool, summary, status, detail } }) as Step;

it("reads edits and commands from the steps the receipt shows", () => {
  const observed = observedWork([
    { ended: true, steps: [
      step("Read", JSON.stringify({ file_path: "src/lib.rs" })),
      step("Edit", JSON.stringify({ file_path: "src/retry.rs" })),
      step("MultiEdit", JSON.stringify({ file_path: "src/retry.rs" })),
      step("Write", JSON.stringify({ file_path: "src/gone.rs" }), "failed"),
      step("Bash", JSON.stringify({ command: "cargo test -p payments" }), "failed"),
      step("Bash", JSON.stringify({ command: "cargo fmt" }), "running"),
    ] },
    { ended: false, steps: [
      step("fileChange", JSON.stringify({ changes: [{ path: "tests/retry.rs" }, { path: "src/retry.rs" }] })),
      step("commandExecution", "npm test", "running"),
    ] },
  ]);
  expect(observed.edits).toEqual({ count: 3, files: ["src/retry.rs", "tests/retry.rs"], unnamed: [] });
  expect(observed.commands.map(c => [c.text, c.status])).toEqual([
    ["cargo test -p payments", "failed"],
    ["cargo fmt", "unknown"],
    ["npm test", "running"],
  ]);
});

it("keeps the step's own line for an edit that names no file", () => {
  const observed = observedWork([{ ended: true, steps: [step("Edit", "+41 −17", "succeeded", "Edited src/retry.rs, tests/retry.rs")] }]);
  expect(observed.edits).toEqual({ count: 1, files: [], unnamed: ["Edited src/retry.rs, tests/retry.rs"] });
  expect(observedWork([{ ended: true, steps: [step("Read", null), step("Task", "{}")] }])).toEqual({ edits: { count: 0, files: [], unnamed: [] }, commands: [] });
});

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });

async function receipt(taskId: string, outcome: Partial<NonNullable<Run["outcome"]>>) {
  const host = window.__sim!.host;
  const run = host.runs.find(r => r.task_id === taskId)!;
  const ended: Run = { ...run, state: "completed", ended_at: Date.now(),
    outcome: { summary: "Done.", changed_files: [], checks: [], still_active: [], unknowns: [], usage: null, ...outcome } };
  host.emit({ type: "run_upserted", run: ended });
  const route = { screen: "receipt", taskId } as const;
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", depth: 1, active: true, route }}><Receipt route={route} /></RouteProvider></AppProvider>);
  await act(async () => { await vi.advanceTimersByTimeAsync(900); });
  return ended;
}

it("says what the steps show when the outcome reports no files or checks", async () => {
  await receipt(ids.tFlaky, {});
  const [edits, commands] = [...document.querySelectorAll<HTMLElement>(".work-observed")];
  expect(within(edits!).getByText(/The agent hasn’t reported changed files\. Steps show 1 edit:/)).toBeTruthy();
  expect(within(edits!).getByRole("listitem").textContent).toBe("Edited src/retry.rs, tests/retry.rs");
  expect(within(commands!).getByText(/The agent hasn’t reported checks\. Steps show 1 command, 1 failed:/)).toBeTruthy();
  expect(within(commands!).getByRole("listitem").textContent).toBe("cargo test -p payments-client retries_on_503Failed");
  expect(screen.queryByText("No changed files reported.")).toBeNull();
  expect(screen.queryByText("No checks reported.")).toBeNull();
});

it("counts commands that never ran by why, in neutral words, not as failed", async () => {
  const host = window.__sim!.host;
  const run = host.runs.find(r => r.task_id === ids.tFlaky)!;
  for (const [n, not_run] of [[1, "denied"], [2, "stopped"]] as const)
    host.emit({ type: "timeline_appended", task_id: ids.tFlaky, item: { seq: host.seq + 1, at: Date.now(), run_id: run.run_id,
      item: { kind: "activity", key: `never-${n}`, tool: "Bash", summary: "Bash finished", status: "failed", not_run, detail: JSON.stringify({ command: `rm -r build-${n}` }) } } });
  await receipt(ids.tFlaky, {});
  const commands = [...document.querySelectorAll<HTMLElement>(".work-observed")][1]!;
  expect(within(commands).getByText(/Steps show 3 commands, 1 failed, 1 denied by you, 1 not run \(stopped\):/)).toBeTruthy();
  expect(within(commands).getAllByRole("listitem").map(li => li.textContent)).toEqual([
    "cargo test -p payments-client retries_on_503Failed", "rm -r build-1Denied by you", "rm -r build-2Not run (stopped)",
  ]);
  expect(commands.querySelectorAll(".work-observed__failed")).toHaveLength(1);
  const counts = [...document.querySelectorAll(".talk-steps__count")].map(c => c.textContent);
  expect(counts.at(-1)).toMatch(/ · 1 denied by you · 1 not run \(stopped\)$/);
  expect(counts.join()).not.toMatch(/[2-9] failed/);
  act(() => { (document.querySelectorAll<HTMLButtonElement>(".talk-steps__head")).forEach(b => b.click()); });
  const icon = (label: string) => document.querySelector(`.talk-steps__row [aria-label="${label}"]`);
  expect(icon("Denied by you")?.classList.contains("talk-steps__icon--failed")).toBe(false);
  expect(icon("Not run (stopped)")?.classList.contains("talk-steps__icon--failed")).toBe(false);
  expect(screen.getByText("Bash: Denied by you")).toBeTruthy();
});

it("keeps No … reported when neither the outcome nor the steps show any", async () => {
  await receipt(workIds.failed, {});
  expect(document.querySelectorAll(".talk-steps__row, .work-observed")).toHaveLength(0);
  expect(screen.getByText("No changed files reported.")).toBeTruthy();
  expect(screen.getByText("No checks reported.")).toBeTruthy();
});

it("shows the reported files and checks rather than the steps when the outcome has them", async () => {
  await receipt(ids.tFlaky, {
    changed_files: [{ path: "crates/payments-client/src/retry.rs", change: "modified", added: 23, removed: 11, provenance: "native" }],
    checks: [{ name: "Payments tests", command: "cargo test -p payments-client", status: "passed", detail: null, provenance: "native" }],
  });
  expect(screen.getByText("crates/payments-client/src/retry.rs")).toBeTruthy();
  expect(screen.getByText("Payments tests")).toBeTruthy();
  expect(screen.queryByText(/Steps show/)).toBeNull();
});
