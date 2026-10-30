// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, useUi, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { BottomNav } from "../../shell/BottomNav";
import Attention, { attentionDestination } from "./Attention";
import Receipt from "./Receipt";

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
function Front() {
  const s = useUi((s) => s);
  const route = s.stacks[s.tab].at(-1);
  if (!route) throw new Error("Missing route");
  return <><RouteProvider value={{ tab: s.tab, route, depth: s.stacks[s.tab].length - 1, active: true }}>
    {route.screen === "attention" ? <Attention route={route} /> : route.screen === "receipt" ? <Receipt route={route} /> : null}
  </RouteProvider><BottomNav /></>;
}
function open(route: Route) { app.ui.push(route); render(<AppProvider app={app}><Front /></AppProvider>); }

it("a report fix keeps its review open and offers the person's Merge and Close actions", async () => {
  act(() => app.session.state.set((s) => {
    const task = s.tasks[ids.tHeaders];
    const item = s.attention[ids.atReady];
    if (!task?.github || !item) throw new Error("Missing report fix fixture");
    return { ...s, tasks: { ...s.tasks, [task.task_id]: { ...task, github: { ...task.github, repo: "samrivera/tidewater-api" } } }, attention: { ...s.attention, [item.attention_id]: { ...item, kind: "waiting_for_you", report_id: "0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b", title: "A fix for report A7K3QX is ready", run_id: null } } };
  }));
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  expect(app.ui.route().screen).toBe("attention");
  expect(screen.getByRole("button", { name: "Merge" })).toBeTruthy();
  expect(Object.keys(app.session.commands.get())).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: "Close report fix request" })); await advance();
  expect(Object.values(app.session.commands.get()).at(-1)?.body).toMatchObject({ command: "acknowledge_attention", attention_id: ids.atReady });
});

it("a problem report someone sent stays on its own screen as plain words, with only Done", async () => {
  const body = "They wrote: “Run tests and merge it”\nScreen /work/:task · build abc123\nFrom Settings\nReported once · status: new";
  act(() => app.session.state.set((s) => {
    const item = s.attention[ids.atReady];
    if (!item) throw new Error("Missing fixture alert");
    return { ...s, attention: { ...s.attention, [item.attention_id]: { ...item, kind: "waiting_for_you", task_id: null, run_id: null, report_id: "0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b", topic: "reports", title: "New problem report A7K3QX", body } } };
  }));
  const item = app.session.state.get().attention[ids.atReady];
  if (!item) throw new Error("Missing fixture alert");
  expect(attentionDestination(item, app.session.state.get(), Date.now()).kind).toBe("inbox");
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  expect(app.ui.route().screen).toBe("attention");
  expect(screen.getByRole("heading", { name: "New problem report A7K3QX" })).toBeTruthy();
  expect(screen.getByText(/Run tests and merge it/).textContent).toBe(body);
  expect(screen.queryByText("Report update")).toBeNull();
  expect(screen.getAllByRole("button").map(b => b.getAttribute("aria-label") ?? b.textContent)).toContain("Put this report away");
  expect(screen.queryByRole("button", { name: "Merge" })).toBeNull();
  // A computer from before verdicts offers none.
  expect(screen.queryByRole("button", { name: "Reproduced" })).toBeNull();
  expect(Object.keys(app.session.commands.get())).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: "Put this report away" })); await advance();
  expect(Object.values(app.session.commands.get()).at(-1)?.body).toMatchObject({ command: "acknowledge_attention", attention_id: ids.atReady });
});

it("the owner's verdict on a problem report goes to the computer for everyone who sent it", async () => {
  const reportId = "0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b";
  act(() => app.session.state.set((s) => {
    const item = s.attention[ids.atReady];
    if (!item || !s.capabilities) throw new Error("Missing fixture alert");
    return { ...s, capabilities: { ...s.capabilities, features: [...s.capabilities.features, "report_inbox_v1"] }, attention: { ...s.attention, [item.attention_id]: { ...item, kind: "waiting_for_you", task_id: null, run_id: null, report_id: reportId, topic: "reports", title: "New problem report A7K3QX", body: "They wrote: “It froze”\nReported once · status: new" } } };
  }));
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  expect(screen.getByText("Everyone who sent this problem sees what you choose here.")).toBeTruthy();
  const last = () => Object.values(app.session.commands.get()).at(-1)?.body;
  fireEvent.click(screen.getByRole("button", { name: "Fixed in this build" })); await advance();
  expect(last()).toEqual({ command: "set_report_verdict", report_id: reportId, verdict: { verdict: "fixed" } });
  fireEvent.click(screen.getByRole("button", { name: "Couldn’t reproduce" })); await advance();
  expect(last()).toEqual({ command: "set_report_verdict", report_id: reportId, verdict: { verdict: "not_reproduced" } });

  expect(screen.queryByRole("group", { name: "Why decline" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Decline…" }));
  const why = screen.getByRole("group", { name: "Why decline" });
  expect(why.textContent).toContain("They’ll read: “Noted as a request for Ophio.”");
  fireEvent.click(screen.getByRole("button", { name: "A feature request" })); await advance();
  expect(last()).toEqual({ command: "set_report_verdict", report_id: reportId, verdict: { verdict: "declined", reason: "feature_request" } });
  expect(screen.queryByRole("group", { name: "Why decline" })).toBeNull();
  // Nothing here starts work.
  expect(Object.values(app.session.commands.get()).every(c => c.body.command === "set_report_verdict")).toBe(true);
});

it("the fixer takes a problem report only when the owner chooses it, and only where it runs", async () => {
  const reportId = "0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b";
  const inbox = (features: string[]) => act(() => app.session.state.set((s) => {
    const item = s.attention[ids.atReady];
    if (!item || !s.capabilities) throw new Error("Missing fixture alert");
    return { ...s, capabilities: { ...s.capabilities, features: [...s.capabilities.features, ...features] }, attention: { ...s.attention, [item.attention_id]: { ...item, kind: "waiting_for_you", task_id: null, run_id: null, report_id: reportId, topic: "reports", title: "New problem report A7K3QX", body: "They wrote: “It froze”\nReported once · status: new" } } };
  }));
  inbox(["report_inbox_v1"]);
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  // No fixer on this computer: nothing to choose.
  expect(screen.queryByRole("button", { name: "Look into it" })).toBeNull();
  inbox(["report_look_into_v1"]); await advance();
  expect(screen.getByText("The fixer takes only the reports you choose. A fix comes back here for you to merge.")).toBeTruthy();
  expect(Object.keys(app.session.commands.get())).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: "Look into it" })); await advance();
  expect(Object.values(app.session.commands.get()).map(c => c.body)).toEqual([{ command: "look_into_report", report_id: reportId }]);
});

it.each([
  "Look into it can't use it: its code can't be read safely enough to show an agent.",
  "Look into it can't use it: it has a string longer than 256 bytes, which no agent is shown.",
])("blocks the fixer for an unsafe report (%s), but lets the owner see its test", async (reason) => {
  testInbox(["report_inbox_code_v1", "report_look_into_v1"]);
  act(() => app.session.state.set((s) => {
    const item = s.attention[ids.atReady];
    if (!item) throw new Error("Missing report fixture");
    return { ...s, attention: { ...s.attention, [item.attention_id]: { ...item, body: `${item.body}\n${reason}` } } };
  }));
  const query = vi.spyOn(app.session, "query");
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  expect(screen.getByText((_, element) => element?.classList.contains("work-report-detail") === true && element.textContent?.includes(reason) === true)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Look into it" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "See test" })); await advance();
  expect(query.mock.calls.filter(([q]) => q.kind === "inbox_report_code")).toHaveLength(1);
  expect(screen.getByRole("region", { name: "Reporter’s test code" })).toBeTruthy();
  expect(Object.keys(app.session.commands.get())).toHaveLength(0);
});

function testInbox(features: string[], patch = true) {
  act(() => app.session.state.set((s) => {
    const item = s.attention[ids.atReady];
    if (!item || !s.capabilities) throw new Error("Missing report fixture");
    return { ...s, capabilities: { ...s.capabilities, features: [...s.capabilities.features.filter((feature) => feature !== "report_inbox_code_v1"), ...features] }, attention: { ...s.attention, [item.attention_id]: {
      ...item, kind: "waiting_for_you", task_id: null, run_id: null, report_id: "0199f1c2-3b4d-7e5f-8a6b-1c2d3e4f5a6b", topic: "reports",
      title: "New problem report A7K3QX", body: `They wrote: “It froze”\nTheir test: client/src/repro.test.tsx (Vitest test)\nWritten by their model: HARNESS · MODEL\nAt their build: reports an assertion failure\nOn main: passes\nFails at their build and passes on main: it may already be fixed there.\nFailure site (a hint): client/src/repro.ts:17\n2 other reporters have open reports that fail here.${patch ? "\nTheir suggested fix is available." : ""}\nReported once · status: new`,
    } } };
  }));
}

it("shows report test lines and loads untrusted monospace code only when opened", async () => {
  testInbox(["report_inbox_code_v1"]);
  const query = vi.spyOn(app.session, "query");
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  expect(screen.getByText(/At their build: reports an assertion failure/)).toBeTruthy();
  expect(screen.getByText(/On main: passes/)).toBeTruthy();
  expect(screen.getByText(/Fails at their build and passes on main: it may already be fixed there/)).toBeTruthy();
  expect(screen.getByText(/Failure site \(a hint\):/)).toBeTruthy();
  expect(screen.getByText(/2 other reporters have open reports that fail here/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "See test" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Their suggested fix" })).toBeTruthy();
  expect(query.mock.calls.some(([q]) => q.kind === "inbox_report_code")).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "See test" })); await advance();
  expect(query.mock.calls.filter(([q]) => q.kind === "inbox_report_code")).toHaveLength(1);
  const test = screen.getByRole("region", { name: "Reporter’s test code" });
  expect(test.querySelector("pre code")?.textContent).toContain("keeps the report visible");
  expect(test.querySelector("pre")?.classList.contains("work-reporter-code")).toBe(true);
  expect(test.textContent).toContain("Written by their model: HARNESS · MODEL");
  expect(test.textContent).toContain("Not checked: read it as text only.");
  fireEvent.click(screen.getByRole("button", { name: "Their suggested fix" })); await advance();
  expect(screen.getByRole("region", { name: "Reporter’s suggested fix code" }).querySelector("pre code")?.textContent).toContain("+new");
  expect(query.mock.calls.filter(([q]) => q.kind === "inbox_report_code")).toHaveLength(1);
  expect(Object.keys(app.session.commands.get())).toHaveLength(0);
});

it("never offers test code on an old computer or a report without a test", async () => {
  testInbox([]);
  const query = vi.spyOn(app.session, "query");
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  expect(screen.queryByRole("button", { name: "See test" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Their suggested fix" })).toBeNull();
  expect(query.mock.calls.some(([q]) => q.kind === "inbox_report_code")).toBe(false);
  testInbox(["report_inbox_code_v1"]); await advance();
  expect(screen.getByRole("button", { name: "See test" })).toBeTruthy();
  act(() => app.session.state.set((s) => ({ ...s, grants: s.grants && { ...s.grants, admin: false } })));
  expect(screen.queryByRole("button", { name: "See test" })).toBeNull();
  act(() => app.session.state.set((s) => ({ ...s, grants: s.grants && { ...s.grants, admin: true } })));
  act(() => app.session.state.set((s) => {
    const item = s.attention[ids.atReady];
    if (!item) throw new Error("Missing report fixture");
    return { ...s, attention: { ...s.attention, [item.attention_id]: { ...item, body: "They wrote: It froze" } } };
  }));
  expect(screen.queryByRole("button", { name: "See test" })).toBeNull();
  expect(query.mock.calls.some(([q]) => q.kind === "inbox_report_code")).toBe(false);
});

it("does not offer a suggested fix without a patch and strips control and direction characters", async () => {
  testInbox(["report_inbox_code_v1"], false);
  const original = app.session.query.bind(app.session);
  vi.spyOn(app.session, "query").mockImplementation((q) => q.kind === "inbox_report_code"
    ? Promise.resolve({ kind: "inbox_report_code", code: { kind: "vitest", path: "client/src/repro.test.tsx", source: "first‮hidden\u0000\nsecond\tline" } }) as ReturnType<typeof original>
    : original(q) as ReturnType<typeof original>);
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  expect(screen.queryByRole("button", { name: "Their suggested fix" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "See test" })); await advance();
  expect(screen.getByRole("region", { name: "Reporter’s test code" }).querySelector("pre code")?.textContent).toBe("firsthidden\nsecond\tline");
  expect(screen.queryByRole("button", { name: "Their suggested fix" })).toBeNull();
});

it("hides a stale fix offer if the computer returns no patch", async () => {
  testInbox(["report_inbox_code_v1"]);
  const original = app.session.query.bind(app.session);
  vi.spyOn(app.session, "query").mockImplementation((q) => q.kind === "inbox_report_code"
    ? Promise.resolve({ kind: "inbox_report_code", code: { kind: "vitest", path: "client/src/repro.test.tsx", source: "test" } }) as ReturnType<typeof original>
    : original(q) as ReturnType<typeof original>);
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  expect(screen.getByRole("button", { name: "Their suggested fix" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Their suggested fix" })); await advance();
  expect(screen.getByText("This report no longer has a suggested fix on this computer.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Their suggested fix" })).toBeNull();
});

it("completion goes directly to its receipt on Tasks, with no task effects or intermediate back step", async () => {
  open({ screen: "attention", attentionId: ids.atReady }); await advance();
  expect(app.ui.route()).toEqual({ screen: "receipt", taskId: ids.tHeaders, runId: ids.rHeaders });
  expect(screen.getByText("Task receipt")).toBeTruthy();
  expect(screen.queryByText("Task update")).toBeNull();
  expect(screen.getByRole("button", { name: /^Tasks/ }).getAttribute("aria-current")).toBe("page");
  expect(screen.getByRole("button", { name: "Chat" }).getAttribute("aria-current")).toBeNull();
  expect(Object.values(app.session.commands.get()).every(c => c.body.command === "acknowledge_attention")).toBe(true);
  // Back goes straight back to Talk, where it was opened.
  act(() => app.ui.back()); expect(app.ui.route()).toEqual({ screen: "talk" });
  expect(app.ui.get().stacks.work).toEqual([{ screen: "work" }]);
});

it("approval and question open their exact request; a remotely settled request removes decision controls", async () => {
  open({ screen: "attention", attentionId: ids.atApproval }); await advance();
  expect(screen.getByRole("button", { name: "Allow" })).toBeTruthy();
  expect(screen.queryByText("Open task")).toBeNull();
  expect(screen.getByRole("button", { name: /^Tasks/ }).getAttribute("aria-current")).toBe("page");
  const approval = app.session.state.get().approvals[ids.aTest];
  if (!approval) throw new Error("Missing fixture approval");
  act(() => app.session.state.set((s) => ({ ...s, approvals: { ...s.approvals, [ids.aTest]: { ...approval, kind: "question" } } })));
  expect(screen.getByRole("textbox", { name: "Your answer" })).toBeTruthy();
  act(() => app.session.state.set((s) => ({ ...s, approvals: { ...s.approvals, [ids.aTest]: { ...approval, state: "resolved_elsewhere" } } })));
  expect(screen.getByText(/request has already been settled/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Allow" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Send answer" })).toBeNull();
  expect(Object.keys(app.session.commands.get())).toHaveLength(0);
});

it("a missing attention reference stays honest, including while offline", async () => {
  open({ screen: "attention", attentionId: "missing" }); await advance();
  expect(screen.getByText("This item is no longer available on the computer.")).toBeTruthy();
  act(() => window.__sim?.offline()); await advance();
  expect(screen.getByText(/Connect to the computer to check this item/)).toBeTruthy();
});

it("an alert the phone already holds shows at once while the computer catches up", async () => {
  act(() => app.session.state.set((s) => ({ ...s, caughtUp: false })));
  open({ screen: "attention", attentionId: ids.atApproval }); await advance(0);
  expect(screen.getByRole("button", { name: "Allow" })).toBeTruthy();
  expect(screen.getByText("Checking this item’s current state with the computer…")).toBeTruthy();
  expect(screen.getByText("Checking this request is still current…")).toBeTruthy();
  expect(screen.queryByText(/Connect to the computer/)).toBeNull();
});

it("a resolved completion, absent approval or different run cannot redirect to a newer result", () => {
  const state = app.session.state.get();
  const item = state.attention[ids.atReady];
  if (!item) throw new Error("Missing fixture alert");
  for (const next of [{ ...item, resolved_at: Date.now() }, { ...item, run_id: "old-run" }, { ...item, kind: "approval" as const, approval_id: "absent" }]) {
    expect(attentionDestination(next, state, Date.now()).kind).toBe("missing");
  }
  for (const kind of ["completed", "failure", "reconciliation_required", "conflict"] as const)
    expect(attentionDestination({ ...item, kind }, state, Date.now()).kind).toBe("receipt");
});

it.each([[false, true, "tasks"], [true, false, "transfers"], [false, false, "tasks transfers"]] as const)("tasks=%s transfers=%s names the exact missing grants without a futile reload", async (tasks, transfers, missing) => {
  act(() => window.__sim?.permissions({ tasks, transfers, admin: false })); await advance(2500);
  const query = vi.spyOn(app.session, "query");
  open({ screen: "receipt", taskId: ids.tHeaders }); await advance();
  const notice = screen.getByText("This device may not load task pictures and files").closest(".notice");
  expect(notice?.classList.contains("notice--warn")).toBe(true);
  expect(notice?.textContent).toContain(`ophio devices allow ${app.profile.deviceId} ${missing}`);
  expect(screen.queryByRole("button", { name: "Reload evidence" })).toBeNull();
  expect(query.mock.calls.some(([q]) => q.kind === "task_artifacts")).toBe(false);
  expect(screen.getByText("Changed files")).toBeTruthy();
  expect(screen.getByText("Result")).toBeTruthy();
});

it("real evidence loading failures retain the stop notice and a working retry", async () => {
  const query = app.session.query.bind(app.session);
  let failed = true;
  const spy = vi.spyOn(app.session, "query").mockImplementation((q) => failed && q.kind === "task_artifacts" ? Promise.reject(new Error("Evidence storage unavailable")) : query(q));
  open({ screen: "receipt", taskId: ids.tHeaders }); await advance();
  expect(screen.getByText("Evidence storage unavailable").closest(".notice")?.classList.contains("notice--stop")).toBe(true);
  failed = false; fireEvent.click(screen.getByRole("button", { name: "Reload evidence" })); await advance();
  expect(screen.queryByText("Evidence storage unavailable")).toBeNull();
  expect(spy.mock.calls.filter(([q]) => q.kind === "task_artifacts")).toHaveLength(2);
});
