// SPDX-License-Identifier: Apache-2.0
import { useState, type ReactNode } from "react";
import { act, cleanup, fireEvent, render, renderHook, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { useCheckActions } from "../../app/checks";
import type { CheckEdit } from "../../protocol/generated/CheckEdit";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { checksIds } from "../../dev/sim/checksFixtures";
import { Toasts } from "../../design/Toast";
import { HostSession } from "../../host/session";
import type { Query } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { PaneContext } from "../../shell/pane";
import { Checks } from "./Checks";
import { checkDuration, checkOutcome, proofWords } from "./words";

let app: App;
const tick = async (ms = 500) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const wrapper = ({ children }: { children: ReactNode }) => <AppProvider app={app}><RouteProvider value={{ tab: "library", route: { screen: "library" }, depth: 1, active: true }}>{children}<Toasts /></RouteProvider></AppProvider>;
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform(), profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await tick(900);
});
afterEach(async () => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose(); app.ui.persistNow();
  await tick(20); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});
const openCheck = async () => {
  render(<Checks projectId={ids.pTide} />, { wrapper }); await tick(800);
  fireEvent.click(screen.getByRole("button", { name: /^Unit tests/ })); await tick();
  return within(screen.getByRole("dialog", { name: "Unit tests" }));
};

it("lists a project's checks and uses the computer's factual lines in the ordered sheet", async () => {
  const dialog = await openCheck();
  expect(dialog.getAllByRole("heading", { level: 3 }).map((h) => h.textContent)).toEqual(["What it checks", "When it runs", "Last result", "Change this check"]);
  expect(dialog.getByText(/After a task changes/)).toBeTruthy();
  expect(dialog.getByText(/Takes about/)).toBeTruthy();
  expect(dialog.getByText("npm test -- --run").closest("details")?.open).toBe(false);
  fireEvent.click(dialog.getByText("Details", { selector: "summary" }));
  expect(dialog.getByText("npm test -- --run")).toBeTruthy();
  expect(dialog.getByText("10 minutes", { selector: "dd" })).toBeTruthy();
  expect(dialog.queryByText("Time limit: 10 minutes")).toBeNull();
  expect(dialog.getByText("Not allowed", { selector: "dd" })).toBeTruthy();
  fireEvent.click(dialog.getByText("Show output", { selector: "summary" }));
  expect(dialog.getByText(/Expected Retry-After/)).toBeTruthy();
});

it("groups checks at Home and keeps projects without checks to one quiet line", async () => {
  render(<Checks projectId={null} />, { wrapper }); await tick(800);
  expect(screen.getByRole("heading", { name: "tidewater-api" })).toBeTruthy();
  expect(screen.getByText(/projects? ha[sv]e? no checks yet/)).toBeTruthy();
  expect(screen.queryByRole("heading", { name: "Notes" })).toBeNull();
});

it("sets up an empty project as a normal task without a Review screen", async () => {
  render(<Checks projectId={ids.pNotes} />, { wrapper }); await tick(800);
  expect(screen.getByText(/Checks are how Ophio/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Set up checks" })); await tick(1000);
  expect(window.__sim!.host.tasks.some((t) => t.project_id === ids.pNotes && t.title === "Set up checks")).toBe(true);
  expect(app.ui.get().tab).toBe("talk");
});

it("does not send any checks query to an older computer", async () => {
  app.session.state.set((s) => ({ ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter((f) => f !== "checks_v1") } }));
  const query = vi.spyOn(app.session, "query");
  render(<Checks projectId={ids.pTide} />, { wrapper }); await tick();
  expect(screen.getByText("This computer needs updating")).toBeTruthy();
  expect(query).not.toHaveBeenCalled();
  await expect(app.session.query({ kind: "checks_list", project_id: null })).rejects.toThrow(/Update this computer/);
  await expect(app.session.command({ command: "checks_run_now", project_id: ids.pTide, check_ids: [] })).rejects.toThrow(/Update this computer/);
});

it("turns a check off immediately and rolls back with a stop toast when refused", async () => {
  const dialog = await openCheck();
  window.__sim!.latency(900);
  window.__sim!.host.checks.refuseNext = "This phone cannot change checks right now.";
  fireEvent.click(dialog.getByRole("button", { name: "Turn off" }));
  expect(dialog.getByRole("button", { name: "Turn on" })).toBeTruthy();
  await tick(2400);
  expect(dialog.getByRole("button", { name: "Turn off" })).toBeTruthy();
  expect(screen.getByText("This phone cannot change checks right now.")).toBeTruthy();
  expect(screen.getByText("This phone cannot change checks right now.").closest(".toast--stop")).toBeTruthy();
});

it("keeps an accepted approval when refreshing its facts fails", async () => {
  const dialog = await openCheck();
  vi.spyOn(app.session, "query").mockRejectedValue(new Error("Could not refresh checks"));
  fireEvent.click(dialog.getByRole("button", { name: "Turn off" }));
  await tick(1800);
  expect(window.__sim!.host.checks.approved.get(ids.pTide)?.checks[0]?.enabled).toBe(false);
  expect(app.session.state.get().checksViews?.[ids.pTide]?.checks[0]?.check.enabled).toBe(false);
  expect(dialog.getByRole("button", { name: "Turn on" })).toBeTruthy();
  expect(screen.getAllByText(/Could not refresh checks/).length).toBeGreaterThan(0);
});

it("restores a refused deletion instead of leaving its row hidden", async () => {
  const dialog = await openCheck();
  window.__sim!.host.checks.refuseNext = "Checks are busy right now.";
  fireEvent.click(dialog.getByRole("button", { name: "Delete" }));
  expect(screen.queryByRole("button", { name: /^Unit tests/ })).toBeNull();
  await tick(1500);
  expect(screen.getByRole("button", { name: /^Unit tests/ })).toBeTruthy();
  expect(screen.getByText("Checks are busy right now.").closest(".toast--stop")).toBeTruthy();
});

it("deletes at once and provides an Undo toast that restores the check", async () => {
  const dialog = await openCheck();
  fireEvent.click(dialog.getByRole("button", { name: "Delete" }));
  expect(screen.queryByRole("button", { name: /^Unit tests/ })).toBeNull();
  await tick(1000);
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  expect(screen.getByRole("button", { name: /^Unit tests/ })).toBeTruthy();
  await tick(1000);
  expect(window.__sim!.host.checks.approved.get(ids.pTide)?.checks.some((c) => c.check_id === checksIds.unit)).toBe(true);
});

it("leaves draft items out individually and applies only the kept positions", async () => {
  render(<Checks projectId={ids.pTide} />, { wrapper }); await tick(800);
  fireEvent.click(screen.getByRole("button", { name: /Suggested checks/ })); await tick(500);
  const dialog = within(screen.getByRole("dialog", { name: "Suggested checks" }));
  expect(dialog.getByText("Now uses the internet")).toBeTruthy();
  expect(dialog.getByText(/Passed on the project as it is now/)).toBeTruthy();
  fireEvent.click(dialog.getAllByRole("button", { name: "Leave out" })[1]!);
  expect(dialog.getByRole("button", { name: "Keep this" })).toBeTruthy();
  fireEvent.click(dialog.getByRole("button", { name: "Use these" })); await tick(1500);
  const approved = window.__sim!.host.checks.approved.get(ids.pTide)!;
  expect(approved.checks.some((c) => c.check_id === checksIds.added)).toBe(true);
  expect(approved.checks.find((c) => c.check_id === checksIds.unit)?.network).toBe(false);
  expect(approved.checks.some((c) => c.check_id === checksIds.links)).toBe(false);
});

it("restores a suggestion after refusal without approving any of its changes", async () => {
  render(<Checks projectId={ids.pTide} />, { wrapper }); await tick(800);
  const version = window.__sim!.host.checks.approved.get(ids.pTide)!.version;
  fireEvent.click(screen.getByRole("button", { name: /Suggested checks/ })); await tick();
  const dialog = within(screen.getByRole("dialog", { name: "Suggested checks" }));
  window.__sim!.host.checks.refuseNext = "The checks changed on another phone.";
  fireEvent.click(dialog.getByRole("button", { name: "Use these" }));
  expect(dialog.getByText("Updating checks…")).toBeTruthy();
  await tick(1600);
  expect(dialog.getByRole("button", { name: "Use these" })).toBeTruthy();
  expect(dialog.getAllByRole("button", { name: "Leave out" })).toHaveLength(3);
  expect(window.__sim!.host.checks.approved.get(ids.pTide)!.version).toBe(version);
  expect(screen.getByText("The checks changed on another phone.").closest(".toast--stop")).toBeTruthy();
});

it("discards a suggestion immediately without changing approved checks", async () => {
  render(<Checks projectId={ids.pTide} />, { wrapper }); await tick(800);
  const version = window.__sim!.host.checks.approved.get(ids.pTide)!.version;
  fireEvent.click(screen.getByRole("button", { name: /Suggested checks/ })); await tick();
  const dialog = within(screen.getByRole("dialog", { name: "Suggested checks" }));
  fireEvent.click(dialog.getByRole("button", { name: "Discard" }));
  expect(dialog.getByText("Discarding suggestion…")).toBeTruthy();
  await tick(1600);
  expect(screen.queryByRole("dialog", { name: "Suggested checks" })).toBeNull();
  expect(screen.queryByRole("button", { name: /Suggested checks/ })).toBeNull();
  expect(window.__sim!.host.checks.approved.get(ids.pTide)!.version).toBe(version);
});

it("never applies a stale draft and asks for a fresh suggestion instead", async () => {
  render(<Checks projectId={ids.pGarden} />, { wrapper }); await tick(800);
  fireEvent.click(screen.getByRole("button", { name: /Suggested checks/ })); await tick(500);
  const dialog = within(screen.getByRole("dialog", { name: "Suggested checks" }));
  expect(dialog.getByText("These checks changed since this suggestion was made")).toBeTruthy();
  expect(dialog.queryByRole("button", { name: "Use these" })).toBeNull();
  fireEvent.click(dialog.getByRole("button", { name: "Ask again" })); await tick(1000);
  expect(window.__sim!.host.tasks.some((t) => t.project_id === ids.pGarden && t.title === "Change checks")).toBe(true);
});

it("keeps hand-edited line breaks and saves then runs the approved check", async () => {
  const dialog = await openCheck();
  fireEvent.click(dialog.getByText("Details", { selector: "summary" }));
  fireEvent.click(dialog.getByRole("button", { name: "Edit by hand" }));
  const patterns = dialog.getByLabelText("Folders and files it watches");
  fireEvent.change(patterns, { target: { value: "src/**\n\ntests/**\n" } });
  expect((patterns as HTMLTextAreaElement).value).toBe("src/**\n\ntests/**\n");
  fireEvent.click(dialog.getByRole("button", { name: "Save and run" })); await tick(2500);
  expect(window.__sim!.host.checks.approved.get(ids.pTide)?.checks[0]?.paths).toEqual(["src/**", "tests/**"]);
});

it("keeps unsaved hand edits to a check when split view folds into one view and widens again", async () => {
  function Side({ split }: { split: boolean }) {
    const [root, setRoot] = useState<HTMLElement | null>(null);
    return <section ref={setRoot}>
      <PaneContext.Provider value={{ id: "left", active: true, visible: true, split, root, width: 500, height: 800 }}><Checks projectId={ids.pTide} /></PaneContext.Provider>
    </section>;
  }
  const view = render(<Side split />, { wrapper }); await tick(800);
  fireEvent.click(screen.getByRole("button", { name: /^Unit tests/ })); await tick();
  const dialog = () => within(screen.getByRole("dialog", { name: "Unit tests" }));
  fireEvent.click(dialog().getByText("Details", { selector: "summary" }));
  fireEvent.click(dialog().getByRole("button", { name: "Edit by hand" }));
  fireEvent.change(dialog().getByLabelText("Name"), { target: { value: "Unit tests, renamed" } });
  fireEvent.change(dialog().getByLabelText("What it runs"), { target: { value: "npm test -- --run --bail" } });

  view.rerender(<Side split={false} />); await tick();
  expect(dialog().getByLabelText<HTMLInputElement>("Name").value).toBe("Unit tests, renamed");
  view.rerender(<Side split />); await tick();
  expect(dialog().getByLabelText<HTMLInputElement>("Name").value).toBe("Unit tests, renamed");
  expect(dialog().getByLabelText<HTMLTextAreaElement>("What it runs").value).toBe("npm test -- --run --bail");
});

it("retries a check-status failure and resumes polling after reconnecting", async () => {
  const dialog = await openCheck();
  const running = { ...window.__sim!.host.checks.jobs.get(checksIds.receipt)!, id: "retry-job", state: "running" as const, task_id: null, run_id: null, results: [], purpose: { kind: "run_now" as const, check_ids: [checksIds.unit] } };
  const finished = { ...running, state: "finished" as const, results: [{ check_id: checksIds.unit, name: "Unit tests", outcome: { kind: "passed" as const }, duration_secs: 1, exit_code: 0, output: "Recovered check output", inputs_changed: [] }] };
  vi.spyOn(app.session, "command").mockResolvedValue({ command_id: "run-retry", status: "accepted", seq: null, result: { kind: "check_job", job: running }, error: null });
  const query = app.session.query.bind(app.session);
  let polls = 0;
  vi.spyOn(app.session, "query").mockImplementation(((q: Query) => {
    if (q.kind !== "check_job" || q.job_id !== running.id) return query(q);
    polls++;
    if (polls === 1) return Promise.reject(new Error("Could not read check status"));
    return Promise.resolve({ kind: "check_job", job: polls === 2 ? { ...running } : finished });
  }) as typeof app.session.query);
  fireEvent.click(dialog.getByRole("button", { name: "Run it now" }));
  await tick(20);
  await tick(1000);
  expect(polls).toBe(1);
  await tick(2100);
  expect(polls).toBe(2);
  act(() => app.session.link.set({ state: "offline", since: Date.now(), reason: "Disconnected", retryAt: null }));
  await tick(3000);
  expect(polls).toBe(2);
  act(() => app.session.link.set({ state: "online", since: Date.now() }));
  await tick(1000);
  expect(polls).toBe(3);
  expect(dialog.getByText("Recovered check output")).toBeTruthy();
  expect(dialog.queryByRole("button", { name: "Stop check" })).toBeNull();
});

const uncertainCheck = async (handSave = false) => {
  const dialog = await openCheck();
  const sim = window.__sim!.host;
  sim.projects.find(project => project.project_id === ids.pTide)!.active_writer = null;
  sim.checks.uncertainStop = true;
  if (handSave) {
    fireEvent.click(dialog.getByText("Details", { selector: "summary" }));
    fireEvent.click(dialog.getByRole("button", { name: "Edit by hand" }));
    fireEvent.click(dialog.getByRole("button", { name: "Save and run" }));
  } else fireEvent.click(dialog.getByRole("button", { name: "Run it now" }));
  await tick(400);
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(dialog.getByRole("button", { name: "Stop check" }));
  await tick(1800);
  expect(command).toHaveBeenCalledWith({ command: "checks_stop_job", job_id: expect.any(String) }, expect.anything());
  expect(dialog.getByText(/Ophio couldn’t confirm that the check stopped/)).toBeTruthy();
  const job = [...sim.checks.jobs.values()].find(job => job.project_id === ids.pTide && job.state === "uncertain")!;
  return { dialog, command, job, sim };
};

it("ordinary Stop never sends confirmation or releases an uncertain check", async () => {
  const { command, job, sim } = await uncertainCheck();
  expect(command.mock.calls.every(([body]) => !("confirmed_stopped" in body))).toBe(true);
  expect(sim.projects.find(project => project.project_id === ids.pTide)?.active_writer).toBe(job.id);
});

it.each([false, true])("only records an uncertain check as stopped after confirmation (hand save: %s)", async (handSave) => {
  const { dialog, command, job, sim } = await uncertainCheck(handSave);
  command.mockClear();
  fireEvent.click(dialog.getByRole("button", { name: "It has stopped" }));
  let confirmation = within(screen.getByRole("dialog", { name: "Has it stopped?" }));
  expect(confirmation.getByText(/record the check as not checked and free the project/)).toBeTruthy();
  expect(confirmation.getByText(/still see the check running, it will refuse/)).toBeTruthy();
  expect(command).not.toHaveBeenCalled();
  fireEvent.click(confirmation.getByRole("button", { name: "Cancel" }));
  expect(command).not.toHaveBeenCalled();
  expect(job.state).toBe("uncertain");
  fireEvent.click(dialog.getByRole("button", { name: "It has stopped" }));
  confirmation = within(screen.getByRole("dialog", { name: "Has it stopped?" }));
  fireEvent.click(confirmation.getByRole("button", { name: "Record as stopped" }));
  await tick(1000);
  expect(command).toHaveBeenCalledExactlyOnceWith({ command: "checks_stop_job", job_id: job.id, confirmed_stopped: true }, expect.anything());
  expect(job.state).toBe("finished");
  expect(sim.projects.find(project => project.project_id === ids.pTide)?.active_writer).toBeNull();
  expect(dialog.queryByRole("button", { name: "It has stopped" })).toBeNull();
  expect(dialog.getByText("Not checked: stopped")).toBeTruthy();
});

it("shows a confirmation refusal and leaves the check and project unchanged", async () => {
  const { dialog, job, sim } = await uncertainCheck();
  sim.checks.uncertainProcessRunning = true;
  const before = structuredClone(job);
  fireEvent.click(dialog.getByRole("button", { name: "It has stopped" }));
  fireEvent.click(within(screen.getByRole("dialog", { name: "Has it stopped?" })).getByRole("button", { name: "Record as stopped" }));
  await tick(1000);
  expect(screen.getByText("Ophio can still see the check running. The project is still reserved.").closest(".toast--stop")).toBeTruthy();
  expect(job).toEqual(before);
  expect(sim.projects.find(project => project.project_id === ids.pTide)?.active_writer).toBe(job.id);
  expect(dialog.getByRole("button", { name: "It has stopped" })).toBeTruthy();
});

it("words result facts without treating stopped or skipped checks as failures", () => {
  expect(checkDuration(72)).toBe("1 minute 12 seconds");
  expect(checkDuration(0)).toBe("0 seconds");
  const result = { check_id: "a", name: "A", outcome: { kind: "stopped" as const }, duration_secs: 0, exit_code: null, output: "", inputs_changed: [] };
  expect(checkOutcome(result)).toBe("Not checked: stopped");
  expect(checkOutcome({ ...result, outcome: { kind: "skipped_paths" } })).toBe("Not needed for these changes");
  expect(proofWords({ ...result, outcome: { kind: "passed" }, duration_secs: 72 })).toBe("Passed on the project as it is now, in 1 minute 12 seconds");
});

it("refuses a hand editor opened before a newer approval without overwriting it", async () => {
  const dialog = await openCheck();
  const version = window.__sim!.host.checks.approved.get(ids.pTide)!.version;
  fireEvent.click(dialog.getByText("Details", { selector: "summary" }));
  fireEvent.click(dialog.getByRole("button", { name: "Edit by hand" }));
  fireEvent.click(dialog.getByRole("button", { name: "Turn off" })); await tick(1400);
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(dialog.getByRole("button", { name: "Save and run" })); await tick(1400);
  expect(command).toHaveBeenCalledWith(expect.objectContaining({ command: "checks_save", base_version: version }), expect.anything());
  expect(window.__sim!.host.checks.approved.get(ids.pTide)!.version).toBe(version + 1);
  expect(window.__sim!.host.checks.approved.get(ids.pTide)!.checks[0]!.enabled).toBe(false);
  expect(screen.getByText("These checks changed since you opened them. Open them again before saving.").closest(".toast--stop")).toBeTruthy();
});

it("never presents the previous definition's facts after a hand save when refresh fails", async () => {
  const dialog = await openCheck();
  fireEvent.click(dialog.getByText("Details", { selector: "summary" }));
  fireEvent.click(dialog.getByRole("button", { name: "Edit by hand" }));
  fireEvent.click(dialog.getByLabelText("Uses the internet"));
  fireEvent.change(dialog.getByLabelText("Time limit in seconds"), { target: { value: "1200" } });
  fireEvent.change(dialog.getByRole("combobox", { name: "When it runs" }), { target: { value: "before_merge" } });
  vi.spyOn(app.session, "query").mockRejectedValue(new Error("Could not refresh checks"));
  fireEvent.click(dialog.getByRole("button", { name: "Save and run" }));
  expect(dialog.queryByText("Does not use the internet")).toBeNull();
  expect(dialog.queryByText(/After a task changes/, { selector: "p" })).toBeNull();
  await tick(1800);
  expect(dialog.queryByText("Does not use the internet")).toBeNull();
  expect(dialog.queryByText(/After a task changes/, { selector: "p" })).toBeNull();
  expect(dialog.queryByText("10 minutes", { selector: "dd" })).toBeNull();
  expect(dialog.getAllByText("Updating…").length).toBeGreaterThan(0);
});

it("keeps approved results separate from proof results even when refreshing fails", async () => {
  render(<Checks projectId={ids.pTide} />, { wrapper }); await tick(800);
  fireEvent.click(screen.getByRole("button", { name: /Suggested checks/ })); await tick();
  const dialog = within(screen.getByRole("dialog", { name: "Suggested checks" }));
  vi.spyOn(app.session, "query").mockRejectedValue(new Error("Could not refresh checks"));
  fireEvent.click(dialog.getByRole("button", { name: "Use these" })); await tick(1800);
  const checks = app.session.state.get().checksViews![ids.pTide]!.checks;
  expect(checks.find(v => v.check.check_id === checksIds.unit)!.last_result!.outcome.kind).toBe("failed");
  expect(checks.find(v => v.check.check_id === checksIds.added)!.last_result).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /^Unit tests/ })); await tick();
  expect(within(screen.getByRole("dialog", { name: "Unit tests" })).getByText(/^Failed after/)).toBeTruthy();
});

it.each([null, "", "   ", "Please suggest a check for the build."])("asks again for stale setup suggestions with recorded request %j", async (request) => {
  const draft = [...window.__sim!.host.checks.drafts.values()].find(d => d.project_id === ids.pGarden)!;
  draft.request = request;
  render(<Checks projectId={ids.pGarden} />, { wrapper }); await tick(800);
  fireEvent.click(screen.getByRole("button", { name: /Suggested checks/ })); await tick();
  const command = vi.spyOn(app.session, "command");
  fireEvent.click(within(screen.getByRole("dialog", { name: "Suggested checks" })).getByRole("button", { name: "Ask again" })); await tick(1000);
  expect(command).toHaveBeenCalledWith({ command: "checks_ask", project_id: ids.pGarden, check_id: null, request: request?.trim() ? request : "Suggest checks for this project again using the latest approved checks." }, expect.anything());
});

it("allows undoing the first approval back to no checks without a version-zero history entry", async () => {
  const sim = window.__sim!.host;
  const check = structuredClone(sim.checks.approved.get(ids.pTide)!.checks[0]!);
  await sim.checks.act({ command: "checks_save", project_id: ids.pNotes, base_version: 0, edit: { kind: "save", check } }, app.session.state.get().grants!);
  expect(sim.checks.history.get(ids.pNotes)!.map(v => v.version)).toEqual([1]);
  render(<Checks projectId={ids.pNotes} />, { wrapper }); await tick(800);
  fireEvent.click(screen.getByRole("button", { name: /^Unit tests/ })); await tick();
  const button = within(screen.getByRole("dialog", { name: "Unit tests" })).getByRole("button", { name: "Undo this change" });
  expect((button as HTMLButtonElement).disabled).toBe(false);
  fireEvent.click(button); await tick(1500);
  expect(sim.checks.approved.get(ids.pNotes)!.checks).toEqual([]);
  expect(screen.getByText("No checks yet")).toBeTruthy();
});

it.each(["running", "cancelling", "uncertain"] as const)("recovers a standalone %s job after reopening the sheet", async (state) => {
  const sim = window.__sim!.host;
  const original = sim.checks.jobs.get(checksIds.receipt)!;
  const job = { ...structuredClone(original), id: "recovered-standalone", task_id: null, run_id: null, state, purpose: { kind: "run_now" as const, check_ids: [checksIds.unit] }, results: [], finished_at: null };
  sim.checks.jobs.set(job.id, job);
  sim.projects.find(p => p.project_id === ids.pTide)!.active_writer = job.id;
  let dialog = await openCheck();
  expect(dialog.getByRole("button", { name: state === "uncertain" ? "It has stopped" : "Stop check" })).toBeTruthy();
  fireEvent.click(dialog.getByRole("button", { name: "Close" })); await tick(100);
  fireEvent.click(screen.getByRole("button", { name: /^Unit tests/ })); await tick();
  dialog = within(screen.getByRole("dialog", { name: "Unit tests" }));
  expect(dialog.getByRole("button", { name: state === "uncertain" ? "It has stopped" : "Stop check" })).toBeTruthy();
  if (state === "uncertain") {
    const command = vi.spyOn(app.session, "command");
    fireEvent.click(dialog.getByRole("button", { name: "It has stopped" }));
    expect(screen.getByRole("dialog", { name: "Has it stopped?" })).toBeTruthy();
    expect(command).not.toHaveBeenCalled();
  }
});

it.each(["save", "set_enabled", "delete", "keep_inputs", "undo"] as const)("sends the caller's captured version for %s, not the latest cached approval", async (kind) => {
  await openCheck();
  const view = app.session.state.get().checksViews![ids.pTide]!;
  const { result } = renderHook(() => useCheckActions(), { wrapper });
  const edit: CheckEdit = kind === "save" ? { kind, check: view.checks[0]!.check }
    : kind === "set_enabled" ? { kind, check_id: checksIds.unit, enabled: false }
    : kind === "undo" ? { kind } : { kind, check_id: checksIds.unit };
  const command = vi.spyOn(app.session, "command");
  let pending: ReturnType<typeof result.current.save>;
  act(() => { pending = result.current.save(ids.pTide, view.approved.version - 1, edit); });
  await tick(1500);
  expect(await pending!).toBe(false);
  expect(command).toHaveBeenCalledWith({ command: "checks_save", project_id: ids.pTide, base_version: view.approved.version - 1, edit }, expect.anything());
  expect(app.session.state.get().checksViews![ids.pTide]!.approved).toEqual(view.approved);
});

it("recovers proof controls without making its proof the approved check's last result", async () => {
  const sim = window.__sim!.host;
  const original = sim.checks.jobs.get(checksIds.receipt)!;
  const proof = { ...structuredClone(original), id: "recovered-proof", task_id: null, run_id: null, state: "uncertain" as const, purpose: { kind: "draft_proof" as const, draft_id: "proof-draft" }, results: original.results.map(r => ({ ...r, outcome: { kind: "passed" as const }, output: "Proof output is separate" })) };
  sim.checks.jobs.set(proof.id, proof);
  sim.projects.find(p => p.project_id === ids.pTide)!.active_writer = proof.id;
  const dialog = await openCheck();
  expect(dialog.getByRole("button", { name: "It has stopped" })).toBeTruthy();
  expect(dialog.getByText(/^Failed after/)).toBeTruthy();
  expect(dialog.queryByText("Proof output is separate")).toBeNull();
});

it.each(["running", "uncertain"] as const)("uses the task's controls for a recovered after-task %s job", async (state) => {
  const sim = window.__sim!.host;
  const original = sim.checks.jobs.get(checksIds.receipt)!;
  const job = { ...structuredClone(original), id: "recovered-after-task", state, results: [] };
  sim.checks.jobs.set(job.id, job);
  sim.projects.find(p => p.project_id === ids.pTide)!.active_writer = job.run_id;
  const dialog = await openCheck();
  const command = vi.spyOn(app.session, "command").mockResolvedValue({ command_id: "stop-task", status: "accepted", seq: null, result: { kind: "check_job", job: { ...job, state: "cancelling" } }, error: null });
  if (state === "running") {
    fireEvent.click(dialog.getByRole("button", { name: "Stop check" })); await tick(20);
    expect(command).toHaveBeenCalledWith({ command: "stop_task", task_id: job.task_id }, expect.anything());
  } else {
    expect(dialog.queryByRole("button", { name: "It has stopped" })).toBeNull();
    fireEvent.click(dialog.getByRole("button", { name: "Open task" }));
    expect(app.ui.get().tab).toBe("talk");
    expect(command).not.toHaveBeenCalled();
  }
});

it.each([false, true])("restores uncertainty confirmation after closing a standalone sheet (hand save: %s)", async (handSave) => {
  const { dialog, job, command } = await uncertainCheck(handSave);
  expect(job.task_id).toBeNull();
  fireEvent.click(dialog.getByRole("button", { name: "Close" })); await tick(100);
  fireEvent.click(screen.getByRole("button", { name: /^Unit tests/ })); await tick(600);
  command.mockClear();
  const reopened = within(screen.getByRole("dialog", { name: "Unit tests" }));
  fireEvent.click(reopened.getByRole("button", { name: "It has stopped" }));
  expect(command).not.toHaveBeenCalled();
  fireEvent.click(within(screen.getByRole("dialog", { name: "Has it stopped?" })).getByRole("button", { name: "Record as stopped" })); await tick(1000);
  expect(command).toHaveBeenCalledExactlyOnceWith({ command: "checks_stop_job", job_id: job.id, confirmed_stopped: true }, expect.anything());
  expect(job.state).toBe("finished");
});
