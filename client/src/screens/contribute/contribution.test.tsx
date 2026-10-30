// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { Command } from "../../protocol/types";
import { Shell } from "../../shell/Shell";
import { Handoffs, chooseFolder, handoffs, resetHandoffs } from "./handoffs";

let app: App;
let sent: Command[];
// Hashing uses real asynchronous work, even while the simulator's timers are fake.
const realNow = Date.now.bind(Date);
const realTimeout = globalThis.setTimeout.bind(globalThis);
// Screens load on first use: let one load, then let what it asks for arrive.
const tick = async (ms = 400) => {
  await act(async () => { await vi.advanceTimersByTimeAsync(ms); await vi.dynamicImportSettled(); });
  await act(async () => { await vi.advanceTimersByTimeAsync(ms); });
};
const button = (name: string | RegExp) => screen.getByRole("button", { name });
const sim = () => window.__sim!;
const activeScreen = () => within(document.querySelector<HTMLElement>(".tab:not(.is-hidden) .route:not(.is-hidden)")!);

/** Wait for an observable state, allowing both real work and simulated replies
 * to finish. Keep timer advances inside act and report the last assertion. */
async function until<T>(read: () => T): Promise<T> {
  const deadline = realNow() + 2000;
  for (;;) {
    await tick(50);
    try { return read(); } catch (error) { if (realNow() >= deadline) throw error; }
    await act(() => new Promise<void>((resolve) => realTimeout(resolve, 10)));
  }
}

const reviewReady = () => until(() => {
  const review = button("Review rules") as HTMLButtonElement;
  expect(review.disabled).toBe(false);
  return review;
});

async function reviewRules() {
  fireEvent.click(await reviewReady());
  await until(() => screen.getByRole("checkbox", { name: /I checked the game’s Clankerhub page just now/ }));
}

async function start({ folder = true }: { folder?: boolean } = {}) {
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother() {}, async rename() {}, async forget() {} };
  app.ui.selectProject(ids.pTide);
  const command = session.command.bind(session);
  sent = [];
  vi.spyOn(session, "command").mockImplementation((body, opts) => { sent.push(body); return command(body, opts); });
  if (folder) chooseFolder(ids.host, ids.rootNotes);
  session.start(); await vi.advanceTimersByTimeAsync(900);
  render(<AppProvider app={app}><Shell /></AppProvider>);
  await tick(500);
}
/** A handoff delivered and opened, as Tasks opens it. */
async function open(over?: Parameters<NonNullable<typeof window.__sim>["handoff"]>[0]) {
  const id = sim().handoff(over)!;
  act(() => app.ui.push({ screen: "contribution", handoffId: id }, "work"));
  await tick(800);
  return id;
}
async function trustAndGet() {
  await reviewRules();
  fireEvent.click(screen.getByRole("checkbox", { name: /I checked the game’s Clankerhub page just now/ }));
  fireEvent.click(button("Trust rules and get source")); await tick();
}
const prepared = () => sent.filter((c) => c.command === "prepare_contribution");

function failHandoffSaves(shouldFail = (_value: string) => true) {
  const setItem = Storage.prototype.setItem;
  return vi.spyOn(Storage.prototype, "setItem").mockImplementation(function (this: Storage, key, value) {
    if (key === "contribution-handoffs.v1" && shouldFail(value)) throw new DOMException("Full", "QuotaExceededError");
    setItem.call(this, key, value);
  });
}

beforeAll(async () => {
  // Compile the screens during setup, outside the first interaction's timeout.
  // Shell still loads them through its real lazy routes when each is opened.
  await Promise.all([
    import("../talk/Talk"), import("../talk/Review"), import("../work/Work"),
    import("./Contribution"), import("./ContributionRules"),
  ]);
});

beforeEach(() => {
  localStorage.clear(); resetHandoffs(); vi.useFakeTimers();
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(() => {
  try {
    cleanup(); app?.session.stop(); window.__sim?.host.dispose();
  } finally {
    vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals();
  }
});

it("checks a handoff on the remembered computer and folder, and downloads nothing until the person confirms", async () => {
  await start();
  const before = sim().host.projects.length;
  await open();
  await reviewReady();
  expect(screen.getByRole("heading", { name: "Open a game from Clankerhub?" })).toBeTruthy();
  expect(screen.getByText("source.example/fixtures/browser")).toBeTruthy();
  expect(screen.getByText("9f2c1d0")).toBeTruthy();
  expect(screen.getByText("parser-edge-case")).toBeTruthy();
  expect(screen.getByText("3 documents · Not reviewed yet")).toBeTruthy();
  expect(screen.getByText(/doesn’t run the game’s scripts or start an agent/)).toBeTruthy();
  expect(sent.map((c) => c.command)).toEqual(["review_contribution"]);
  expect(sim().host.projects).toHaveLength(before);

  await reviewRules();
  expect(screen.getByRole("heading", { name: "Review the game’s rules" })).toBeTruthy();
  expect(screen.getByText("AGENTS.md")).toBeTruthy();
  expect(screen.getByText(/can’t ask Clankerhub yet whether this handoff is genuine/)).toBeTruthy();
  const go = button("Trust rules and get source") as HTMLButtonElement;
  // Unavailable but still focusable, with its reason readable and as its description.
  expect(go.getAttribute("aria-disabled")).toBe("true");
  expect(go.disabled).toBe(false);
  const reason = document.getElementById(go.getAttribute("aria-describedby")!)!;
  expect(reason.textContent).toBe("Check the listing on Clankerhub first.");
  fireEvent.click(go); await tick();
  expect(prepared()).toHaveLength(0);
});

it("reads pinned rules before trust as inert text and creates no project", async () => {
  await start();
  const before = sim().host.projects.length;
  await open();
  await reviewRules();
  fireEvent.click(button("Read AGENTS.md")); await tick();
  const text = screen.getByLabelText("AGENTS.md");
  expect(text.tagName).toBe("PRE");
  expect(text.textContent).toContain("<script>Never execute document content.</script>");
  expect(text.querySelector("script")).toBeNull();
  expect(screen.getByText(/Agent instructions · \d+ bytes/)).toBeTruthy();
  expect(prepared()).toHaveLength(0);
  expect(sim().host.projects).toHaveLength(before);
  expect((screen.getByRole("checkbox") as HTMLInputElement).checked).toBe(false);
  fireEvent.click(button("Hide AGENTS.md")); await tick();
  expect(screen.queryByLabelText("AGENTS.md")).toBeNull();
});

it("reads two slow policies on one serial connection without timing out the second", async () => {
  await start();
  await open();
  await reviewRules();
  const query = sim().host.query.bind(sim().host);
  let tail = Promise.resolve();
  vi.spyOn(sim().host, "query").mockImplementation((q) => {
    if (q.kind !== "contribution_policy") return query(q);
    // The real companion runs requests serially. Each download takes less
    // than the query timeout, but their combined time exceeds it.
    const result = tail.then(async () => {
      await new Promise((resolve) => setTimeout(resolve, 12_000));
      return query(q);
    });
    tail = result.then(() => undefined);
    return result;
  });
  fireEvent.click(button("Read AGENTS.md"));
  fireEvent.click(button("Read CONTRIBUTING.md"));
  await tick(12_500);
  expect(screen.getByLabelText("AGENTS.md")).toBeTruthy();
  expect(screen.getByLabelText("CONTRIBUTING.md")).toBeTruthy();
  expect(screen.queryByText(/Couldn’t read and verify this file/)).toBeNull();
  expect(prepared()).toHaveLength(0);
});

it("discards an unsent policy read when the rules screen is closed", async () => {
  await start();
  await open();
  await reviewRules();
  const query = sim().host.query.bind(sim().host);
  const paths: string[] = [];
  vi.spyOn(sim().host, "query").mockImplementation(async (q) => {
    if (q.kind === "contribution_policy") {
      paths.push(q.path);
      await new Promise((resolve) => setTimeout(resolve, 12_000));
    }
    return query(q);
  });
  fireEvent.click(button("Read AGENTS.md"));
  fireEvent.click(button("Read CONTRIBUTING.md"));
  await tick(100);
  act(() => app.ui.finish(["contribution_rules"]));
  await tick(12_500);
  expect(paths).toEqual(["AGENTS.md"]);
  expect(prepared()).toHaveLength(0);
});

it("can retry a failed policy read without granting trust", async () => {
  await start();
  await open();
  await reviewRules();
  vi.spyOn(sim().host.contribution, "policy").mockImplementationOnce(() => { throw new Error("Digest mismatch"); });
  fireEvent.click(button("Read AGENTS.md")); await tick();
  expect(screen.getByText(/Couldn’t read and verify this file/)).toBeTruthy();
  expect(screen.queryByLabelText("AGENTS.md")).toBeNull();
  fireEvent.click(button("Read AGENTS.md")); await tick();
  expect(screen.getByLabelText("AGENTS.md")).toBeTruthy();
  expect(prepared()).toHaveLength(0);
});

it("does not ask an older receiver for policy previews", async () => {
  await start();
  const caps = sim().host.contribution.capabilities();
  vi.spyOn(sim().host.contribution, "capabilities").mockReturnValue({ ...caps, capabilities: ["pinned-import"] });
  await open();
  await reviewRules();
  expect(screen.queryByRole("button", { name: "Read AGENTS.md" })).toBeNull();
  expect(screen.getByText(/Update studio to read rules here/)).toBeTruthy();
});

it("gets the source on the person's own check, follows it to a paused project, and starts nothing", async () => {
  await start();
  await open();
  await trustAndGet();
  const body = prepared()[0] as Extract<Command, { command: "prepare_contribution" }>;
  expect(body.approval.publisher_id).toBe("clankerhub");
  expect(body.approval.game_id).toBe("fixture-browser");
  expect(body.approval.checked_at).toBeLessThanOrEqual(Math.floor(Date.now() / 1000) - 30);
  expect(body.approval.valid_until - body.approval.checked_at).toBeLessThanOrEqual(300);
  expect(screen.getByRole("heading", { name: "Getting the source" })).toBeTruthy();
  expect(screen.getByText("Downloading the source")).toBeTruthy();

  await tick(2600); await tick(1600); await tick(1600);
  const project = sim().host.projects.find((p) => p.name === "fixture-browser")!;
  expect(project).toBeTruthy();
  expect(screen.getByRole("heading", { name: "fixture-browser is ready" })).toBeTruthy();
  expect(screen.getByText("Nothing has run yet")).toBeTruthy();
  expect(screen.getByText("Queue paused")).toBeTruthy();
  expect(screen.getByText(/can’t bring in this task’s details yet. Opening it didn’t claim it/)).toBeTruthy();
  expect(sent.some((c) => c.command === "create_task" || c.command === "queue_resume")).toBe(false);

  // Planning opens Review in Plan; nothing starts until Start there.
  fireEvent.click(button("Plan something")); await tick();
  expect(app.ui.get().tab).toBe("talk");
  expect(app.ui.route()).toMatchObject({ screen: "review", projectId: project.project_id, taskId: null });
  expect(app.ui.draft(project.project_id, null)?.mode).toBe("plan");
  expect(sent.some((c) => c.command === "create_task")).toBe(false);
  expect(handoffs().get()).toHaveLength(0);
});

it("still shows the project once the person's check has lapsed", async () => {
  await start();
  await open();
  await trustAndGet();
  await tick(2600); await tick(1600); await tick(1600);
  const h = handoffs().get()[0]!;
  const projectId = h.view!.project_id;
  expect(h.view!.code).toBe("prepared");
  expect(projectId).toBeTruthy();
  act(() => app.ui.finish(["contribution"]));
  // Expire the check without running minutes of unrelated UI/heartbeat timers.
  vi.setSystemTime(Date.now() + 6 * 60_000);
  const approval = (prepared()[0] as Extract<Command, { command: "prepare_contribution" }>).approval;
  expect(Date.now() / 1000).toBeGreaterThan(approval.valid_until);
  expect(sim().host.contribution.view(h.operationId!).code).toBe("freshness_unknown");
  act(() => app.ui.push({ screen: "contribution", handoffId: h.id }, "work")); await tick(800);
  expect(handoffs().find(h.id)?.view).toMatchObject({ code: "freshness_unknown", project_id: projectId });
  expect(screen.getByRole("heading", { name: "fixture-browser is ready" })).toBeTruthy();
});

it("cancels a download, says what stays on the computer, and never says nothing was downloaded", async () => {
  await start();
  sim().host.contribution.hold = true;
  await open();
  await trustAndGet();
  fireEvent.click(button("Cancel")); await tick(100);
  expect(screen.getByText("Waiting for studio to confirm it stopped")).toBeTruthy();
  expect((button("Done") as HTMLButtonElement).disabled).toBe(true);
  await tick(1000); await tick(1600);
  expect(screen.getByRole("heading", { name: "Canceled" })).toBeTruthy();
  expect(screen.getByText(/Anything it got so far stays in its own folder on studio. Nothing was added or run/)).toBeTruthy();
  expect(screen.queryByText(/Nothing was downloaded/)).toBeNull();
  fireEvent.click(button("Done")); await tick();
  expect(handoffs().get()).toHaveLength(0);
});

it("leaves an existing workspace at another revision untouched and offers another folder", async () => {
  await start();
  await open();
  await trustAndGet();
  await tick(2600); await tick(1600); await tick(1600);
  fireEvent.click(button("Done")); await tick();
  const projects = sim().host.projects.length;
  await open({ commit: "1111111111111111111111111111111111111111" });
  await trustAndGet();
  expect(screen.getByRole("heading", { name: "You already have this game in Notes" })).toBeTruthy();
  expect(screen.getByText(/never changes files in a workspace you already have/)).toBeTruthy();
  expect(sim().host.projects).toHaveLength(projects);
  fireEvent.click(button("Choose another folder")); await tick();
  expect(screen.getByRole("dialog", { name: "Where should contribution projects go?" })).toBeTruthy();
});

it("asks for a folder first when none is remembered", async () => {
  await start({ folder: false });
  await open();
  expect(sent).toHaveLength(0);
  fireEvent.click(button("Choose a folder")); await tick();
  fireEvent.click(within(screen.getByRole("radiogroup", { name: "Folder" })).getByRole("radio", { name: /^Notes/ }));
  fireEvent.click(button("Use this folder")); await tick(800);
  await reviewReady();
  expect(sent.map((c) => c.command)).toEqual(["review_contribution"]);
  expect(button("Review rules")).toBeTruthy();
});

it("shows a receiver result it doesn't know as something unexpected, with its code for reporting", async () => {
  await start();
  sim().host.contribution.next.review = "future_state" as never;
  await open();
  await until(() => expect(screen.getByRole("heading", { name: "Something unexpected" })).toBeTruthy());
  expect(screen.getByText("future_state")).toBeTruthy();
});

it("waits under Tasks until opened", async () => {
  await start();
  sim().handoff();
  act(() => app.ui.selectTab("work")); await tick(800);
  fireEvent.click(button("Open fixture-browser from Clankerhub")); await tick(800);
  await reviewReady();
  expect(screen.getByRole("heading", { name: "Open a game from Clankerhub?" })).toBeTruthy();
});

it("sends nothing to a computer whose Øphio has no receiver", async () => {
  await start();
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter((f) => f !== "contribution_import_v1") } })));
  const query = vi.spyOn(app.session, "query");
  await open();
  expect(screen.getByText("Update Øphio on studio")).toBeTruthy();
  expect(query.mock.calls.some(([q]) => q.kind.startsWith("contribution"))).toBe(false);
  expect(sent).toHaveLength(0);
});

it("says the computer can't be reached and sends nothing while offline", async () => {
  await start();
  sim().offline();
  await tick(500);
  await open();
  expect(screen.getByText("Can’t reach studio")).toBeTruthy();
  expect(screen.getByText(/Nothing was downloaded/)).toBeTruthy();
  expect(sent).toHaveLength(0);
  // Back online, it checks the handoff by itself (that runs nothing), and still downloads nothing.
  sim().online(); await tick(1500);
  await reviewReady();
  expect(sent.map((c) => c.command)).toEqual(["review_contribution"]);
  expect(button("Review rules")).toBeTruthy();
});

// ---- Review findings on #145 ---------------------------------------------------

it("blocks a download when its recovery record cannot be saved, keeps it visible, and permits an explicit retry", async () => {
  await start();
  const id = await open();
  await reviewRules();
  const writes = failHandoffSaves();
  fireEvent.click(screen.getByRole("checkbox", { name: /I checked the game’s Clankerhub page just now/ }));
  fireEvent.click(button("Trust rules and get source")); await tick(300);
  expect(prepared()).toHaveLength(0);
  expect(handoffs().find(id)).toMatchObject({ unsettled: null, prepareCommand: null });
  expect(activeScreen().getByText("Recovery details aren’t saved")).toBeTruthy();
  // No request was sent, so the old approval record is safe after a restart.
  expect(new Handoffs().find(id)).toMatchObject({ unsettled: null, prepareCommand: null, started: false });
  act(() => app.ui.finish(["contribution_rules"])); await tick();
  expect(activeScreen().getByText("Recovery details aren’t saved")).toBeTruthy();
  act(() => app.ui.finish(["contribution"])); await tick();
  act(() => handoffs().prune(Date.now() + 3_600_000));
  expect(handoffs().find(id)).toBeTruthy();
  expect(handoffs().expiredRemoved).toBe(0);
  expect(button("Open fixture-browser from Clankerhub")).toBeTruthy();
  expect(activeScreen().getByText("Recovery details aren’t saved")).toBeTruthy();

  writes.mockRestore();
  fireEvent.click(activeScreen().getByRole("button", { name: "Try saving again" })); await tick();
  expect(screen.queryByText("Recovery details aren’t saved")).toBeNull();
  expect(prepared()).toHaveLength(0);
  fireEvent.click(button("Open fixture-browser from Clankerhub")); await tick(800);
  await trustAndGet();
  expect(prepared()).toHaveLength(1);
});

it.each([false, true])("retains a durably saved download across restart when later saves fail (lost receipt: %s)", async (lost) => {
  await start();
  const id = await open();
  await reviewRules();
  const command = vi.mocked(app.session.command).getMockImplementation()!;
  vi.spyOn(app.session, "command").mockImplementation((body, opts) => {
    if (body.command === "prepare_contribution") {
      // Inspect what a restarted phone would load at the moment of dispatch.
      expect(new Handoffs().find(id)).toMatchObject({ unsettled: opts!.commandId, prepareCommand: opts!.commandId });
      // Allow smaller writes, but reject the downloaded state. Clearing the
      // command in a separate earlier save would lose recovery on restart.
      failHandoffSaves((value) => JSON.parse(value).some((h: { id: string; started: boolean }) => h.id === id && h.started));
    }
    return command(body, opts);
  });
  if (lost) sim().dropNextReceipt();
  fireEvent.click(screen.getByRole("checkbox", { name: /I checked the game’s Clankerhub page just now/ }));
  fireEvent.click(button("Trust rules and get source")); await tick(300);
  expect(prepared()).toHaveLength(1);
  if (!lost) {
    expect(handoffs().find(id)).toMatchObject({ started: true, unsettled: null });
    expect(activeScreen().getByText("Recovery details aren’t saved")).toBeTruthy();
  }
  sim().offline();
  cleanup();
  resetHandoffs();
  const restored = handoffs().find(id)!;
  expect(restored.unsettled).toBeTruthy();
  expect(restored.unsettled).toBe(restored.prepareCommand);
  handoffs().prune(restored.expiresAt! + 1);
  expect(handoffs().find(id)).toBeTruthy();
  expect(handoffs().expiredRemoved).toBe(0);
});

/** The fingerprint shown for the person to compare, without its spacing. */
const fingerprint = () => screen.getByText("Handoff fingerprint").nextElementSibling!.textContent!.replace(/\s/g, "");

it("asks the person to compare a fingerprint that changes with the rule files, and sends that one", async () => {
  await start();
  await open();
  await reviewRules();
  const first = fingerprint();
  expect(first).toMatch(/^[0-9a-f]{64}$/);
  expect(screen.getByText(/If the page doesn’t show the same fingerprint, don’t trust this handoff/)).toBeTruthy();
  act(() => app.ui.finish(["contribution", "contribution_rules"]));
  handoffs().get().forEach((h) => handoffs().remove(h.id));
  // The same game, release, source and revision, but other rule files.
  await open({ instructions: "OTHER.md" });
  await reviewRules();
  expect(screen.getByText("OTHER.md")).toBeTruthy();
  const second = fingerprint();
  expect(second).not.toBe(first);
  fireEvent.click(screen.getByRole("checkbox", { name: /all of it matches/ }));
  fireEvent.click(button("Trust rules and get source")); await tick();
  const body = prepared()[0] as Extract<Command, { command: "prepare_contribution" }>;
  expect(body.approval.handoff_sha256).toBe(second);
});

it("shows an unresolved download when returning from Rules offline after losing its receipt", async () => {
  await start();
  sim().host.contribution.hold = true;
  const id = await open();
  await reviewRules();
  fireEvent.click(screen.getByRole("checkbox", { name: /I checked the game’s Clankerhub page just now/ }));
  sim().dropNextReceipt();
  fireEvent.click(button("Trust rules and get source")); await tick(300);
  act(() => sim().offline());
  fireEvent.click(button("Back")); await tick();

  const h = handoffs().find(id)!;
  expect(prepared()).toHaveLength(1);
  expect(h.unsettled).not.toBeNull();
  expect(h.unsettled).toBe(h.prepareCommand);
  expect(sim().host.contribution.view(h.operationId!).code).toBe("acquiring");
  expect(screen.getByRole("heading", { name: "Open a game from Clankerhub?" })).toBeTruthy();
  expect(screen.queryByText(/Nothing was downloaded/)).toBeNull();
  expect(screen.getByText("No answer yet")).toBeTruthy();
  expect(screen.getByText(/studio may have started getting the source/)).toBeTruthy();
  expect(screen.getByText(/won’t start a second download/)).toBeTruthy();
  expect((button("Review rules") as HTMLButtonElement).disabled).toBe(true);

  act(() => sim().online());
  await until(() => expect(handoffs().find(id)?.unsettled).toBeNull());
  expect(screen.getByRole("heading", { name: "Getting the source" })).toBeTruthy();
  expect(screen.queryByText("No answer yet")).toBeNull();
  expect(screen.queryByText(/Nothing was downloaded/)).toBeNull();
  expect(prepared()).toHaveLength(1);
});

it("keeps a download request whose answer was lost, asks the computer about it, and never says nothing was downloaded", async () => {
  await start();
  await open();
  await reviewRules();
  fireEvent.click(screen.getByRole("checkbox", { name: /I checked the game’s Clankerhub page just now/ }));
  sim().dropNextReceipt();
  fireEvent.click(button("Trust rules and get source")); await tick(300);
  const h = handoffs().get()[0]!;
  expect(h.unsettled).not.toBeNull();
  expect(h.unsettled).toBe(h.prepareCommand);
  // Long past the handoff's expiry, it stays: it may have downloaded something.
  handoffs().prune(Date.now() + 3_600_000);
  expect(handoffs().find(h.id)).toBeTruthy();
  expect(screen.getByText(/may have started. Go back to the handoff/)).toBeTruthy();
  // The phone notices the silent connection and reconnects; the computer then
  // says the download started, and the phone follows it.
  for (let i = 0; i < 20; i++) await tick(2000);
  expect(handoffs().find(h.id)?.unsettled).toBeNull();
  expect(prepared()).toHaveLength(1);
  expect(sim().host.projects.some((p) => p.name === "fixture-browser")).toBe(true);
  act(() => app.ui.finish(["contribution_rules"])); await tick();
  expect(screen.getByRole("heading", { name: "fixture-browser is ready" })).toBeTruthy();
});

it("ignores a reply about a folder the person has since changed", async () => {
  await start();
  sim().latency(1500);
  await open();
  // The check for Notes is still on its way when the person picks another folder.
  await until(() => expect(screen.getByText("studio is checking the handoff")).toBeTruthy());
  const ophio = "0192f7a1-0000-7000-8000-000000000905";
  fireEvent.click(button(/^Folder: Notes/)); await act(async () => { await vi.advanceTimersByTimeAsync(50); });
  fireEvent.click(within(screen.getByRole("radiogroup", { name: "Folder" })).getByRole("radio", { name: /^Ophio/ }));
  fireEvent.click(button("Use this folder"));
  await tick(2000); await tick(2000);
  await until(() => {
    const h = handoffs().get()[0]!;
    const op = sim().host.contribution.ops.get(h.operationId!)!;
    expect(op?.input.root_id).toBe(ophio);
  });
});

it("ignores a progress answer sent before the person canceled", async () => {
  await start();
  sim().host.contribution.hold = true;
  sim().host.contribution.stopMs = 20_000;
  await open();
  await trustAndGet();
  // A progress check is slow to come back; Cancel's answer overtakes it, and
  // the stop is still waiting to be confirmed when the old answer lands.
  sim().latency(4000);
  await act(async () => { await vi.advanceTimersByTimeAsync(1600); });
  sim().latency(0);
  fireEvent.click(button("Cancel"));
  for (let t = 0; t < 6000; t += 200) {
    await act(async () => { await vi.advanceTimersByTimeAsync(200); });
    expect(handoffs().get()[0]!.view?.code, `at ${t} ms`).toBe("canceled");
  }
  expect(screen.queryByRole("heading", { name: "Getting the source" })).toBeNull();
});

it("sends no contribution request for a remembered handoff to a computer whose Øphio lacks the receiver", async () => {
  await start();
  await open();
  await reviewReady();
  sim().host.contribution.hold = true;
  const id = handoffs().get()[0]!.id;
  act(() => app.ui.finish(["contribution"]));
  const awaiting = handoffs().find(id)!.view!;
  act(() => app.session.state.set((s) => ({ ...s, capabilities: { ...s.capabilities!, features: s.capabilities!.features.filter((f) => f !== "contribution_import_v1") } })));
  const query = vi.spyOn(app.session, "query");
  sent.length = 0;
  // A download remembered as running is not polled.
  act(() => handoffs().update(id, { view: { ...awaiting, code: "acquiring" } }));
  act(() => app.ui.push({ screen: "contribution", handoffId: id }, "work")); await tick(3000);
  expect(query.mock.calls.some(([q]) => q.kind.startsWith("contribution"))).toBe(false);
  // Nor is one remembered as waiting for approval prepared.
  act(() => handoffs().update(id, { view: awaiting }));
  act(() => app.ui.push({ screen: "contribution_rules", handoffId: id }, "work")); await tick();
  fireEvent.click(screen.getByRole("checkbox", { name: /I checked the game’s Clankerhub page just now/ }));
  fireEvent.click(button("Trust rules and get source")); await tick();
  expect(sent).toHaveLength(0);
});

it.each([0, 100])("shows Next up as it is when reopening a project whose queue is running (hashing delayed by %i ms)", async (delay) => {
  if (delay) {
    const digest = crypto.subtle.digest.bind(crypto.subtle);
    vi.spyOn(crypto.subtle, "digest").mockImplementation(async (...args) => {
      const result = await digest(...args);
      await new Promise<void>((resolve) => realTimeout(resolve, delay));
      return result;
    });
  }
  await start();
  await open();
  await trustAndGet();
  await tick(2600); await tick(1600); await tick(1600);
  const project = sim().host.projects.find((p) => p.name === "fixture-browser")!;
  expect(screen.getByText("Queue paused")).toBeTruthy();
  // Resumed elsewhere while Ready is showing.
  act(() => sim().host.talk.put({ ...sim().host.talk.queue(project.project_id), paused: false, revision: 9 }));
  await tick();
  expect(screen.getByText("Queue running")).toBeTruthy();
  fireEvent.click(button("Done")); await tick();
  // Opening the game again keeps the running queue, and says so.
  await open();
  await trustAndGet();
  await tick(1600); await tick(1600);
  expect(screen.getByRole("heading", { name: "Back in fixture-browser" })).toBeTruthy();
  expect(screen.getByText("Queue running")).toBeTruthy();
  expect(screen.getByText("Next up is running in this project")).toBeTruthy();
  expect(screen.queryByText("Queue paused")).toBeNull();
});
