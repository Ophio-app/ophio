// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import type { Approval, TimelineBody, TimelineItem } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import Attention from "./AttentionScreen";
import { AttentionList } from "./shared";

let app: App;
let asked: Approval;
let history: TimelineItem[];
const advance = async (ms = 600) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
const answer = "Give me more detail before I choose. This is not permission to start.";
const explanation = "The first choice preserves the existing data. The second choice changes how future work is saved.";
const final = "Choose the first or second option.";
beforeEach(async () => {
  vi.useFakeTimers(); localStorage.clear();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, profiles: [profile], session, ui: new UiStore(profile.profileId), switchTo: vi.fn(), pairAnother: vi.fn(), rename: vi.fn(), forget: async () => {} };
  session.start(); await advance(900);
  const host = window.__sim!.host;
  asked = { ...host.approvals.find(a => a.approval_id === ids.aTest)!, approval_id: "0192f7a1-0000-7000-8000-00000000ab16", kind: "question", title: "Which approach should I use?", detail: "Please choose an approach.", questions: undefined, raw_request: null };
  host.approvals = host.approvals.map(a => a.approval_id === ids.aTest ? asked : a);
  host.attention = host.attention.map(a => a.attention_id === ids.atApproval ? { ...a, approval_id: asked.approval_id, title: asked.title } : a);
  act(() => {
    host.emit({ type: "approval_upserted", approval: asked });
    host.emit({ type: "attention_upserted", item: host.attention.find(a => a.attention_id === ids.atApproval)! });
  });
  await advance();
  history = [{ seq: 10000, at: asked.requested_at, run_id: asked.run_id, item: { kind: "approval", approval_id: asked.approval_id } }];
  const original = host.query.bind(host);
  vi.spyOn(host, "query").mockImplementation(async q => {
    if (q.kind !== "task_timeline" || q.task_id !== asked.task_id) return original(q);
    const items = history.filter(i => q.before_seq === null || i.seq < q.before_seq);
    return { kind: "task_timeline", items: items.slice(-q.limit), has_more: items.length > q.limit };
  });
});
afterEach(() => {
  cleanup(); app.session.stop(); window.__sim?.host.dispose();
  vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks();
});

function open(entry: "alert" | "sheet") {
  const route = entry === "alert" ? { screen: "attention", attentionId: ids.atApproval } as const : { screen: "work" } as const;
  app.ui.push(route, "work");
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 1, active: true }}>
    {entry === "alert" ? <Attention route={{ screen: "attention", attentionId: ids.atApproval }} /> : <AttentionList projectId={ids.pTide} />}
  </RouteProvider></AppProvider>);
  if (entry === "sheet") fireEvent.click(screen.getByText(/^Approval needed/).closest("button")!);
}
function append(body: TimelineBody, at = Date.now(), runId: string | null = asked.run_id) {
  const item = { seq: (history.at(-1)?.seq ?? 10000) + 1, at, run_id: runId, item: body };
  history.push(item);
  act(() => window.__sim!.host.emit({ type: "timeline_appended", task_id: asked.task_id, item }));
}
function settle(change: Partial<Approval> = {}) {
  const approved: Approval = { ...asked, state: "allowed", resolved_at: Date.now(), revision: asked.revision + 1,
    delivery: { decision: { decision: "answer", text: answer }, at: Date.now(), error: null }, ...change };
  const host = window.__sim!.host;
  host.approvals = host.approvals.map(a => a.approval_id === asked.approval_id ? approved : a);
  act(() => {
    host.emit({ type: "approval_upserted", approval: approved });
    host.emit({ type: "attention_resolved", attention_id: ids.atApproval });
  });
}
async function sendAnswer() {
  fireEvent.change(screen.getByRole("textbox", { name: "Your answer" }), { target: { value: answer } });
  fireEvent.click(screen.getByRole("button", { name: "Send answer" }));
  await advance(1200);
}

for (const entry of ["alert", "sheet"] as const) describe(entry, () => {
  it("keeps the question, the person's answer and the full explanation beside a later short final", async () => {
    open(entry);
    const load = vi.spyOn(app.session, "loadTimeline");
    await sendAnswer();
    expect(screen.getByText(asked.title)).toBeTruthy();
    expect(screen.getByText(`Your answer: ${answer}`)).toBeTruthy();
    expect(screen.getByText("Waiting for agent’s reply…")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Send answer" })).toBeNull();
    expect(load).toHaveBeenCalledWith(asked.task_id);
    expect(app.session.state.get().timelineReaders[asked.task_id]).toBeGreaterThan(0);
    append({ kind: "agent_message", text: `## Why these choices\n\n${explanation}`, partial: false });
    await advance();
    append({ kind: "agent_message", text: final, partial: false });
    append({ kind: "run_ended", run_id: asked.run_id, state: "completed" });
    await advance();
    const reply = within(screen.getByRole("region", { name: "Agent’s reply" }));
    expect(reply.getByRole("heading", { name: "Why these choices" })).toBeTruthy();
    expect(reply.getByText(explanation)).toBeTruthy();
    expect(reply.getByText(final)).toBeTruthy();
    expect(reply.queryByText("Waiting for agent’s reply…")).toBeNull();
    // The only submitted command is an answer, never permission or a resume.
    expect(Object.values(app.session.commands.get()).map(c => c.body)).toEqual([
      { command: "resolve_approval", approval_id: asked.approval_id, decision: { decision: "answer", text: answer } },
    ]);
    fireEvent.click(reply.getByRole("button", { name: "Open task" }));
    expect(app.ui.get().taskByProject[asked.project_id]).toBe(asked.task_id);
    cleanup();
    expect(app.session.state.get().timelineReaders[asked.task_id] ?? 0).toBe(0);
  });

  it("opens explanation links through existing external and project-file navigation without changing grants or resuming work", async () => {
    open(entry);
    settle();
    const root = app.session.state.get().libraryRoots.find(r => r.kind === "project" && r.project_id === asked.project_id)!;
    expect(root).toBeTruthy();
    const external = vi.spyOn(app.platform, "openExternal").mockResolvedValue(undefined);
    const grants = structuredClone(app.session.state.get().grants);
    append({ kind: "agent_message", text: `[Read the explanation](https://example.com/choices)\n\n[Read project notes](${root.host_path}/docs/answers.md:12)\n\n[Unsafe link](javascript:alert%281%29)`, partial: false }, Date.now() + 1);
    await advance();
    const reply = within(screen.getByRole("region", { name: "Agent’s reply" }));
    fireEvent.click(reply.getByRole("link", { name: "Read the explanation" }));
    expect(external).toHaveBeenCalledExactlyOnceWith("https://example.com/choices");
    fireEvent.click(reply.getByRole("link", { name: "Read project notes" }));
    expect(app.ui.route()).toEqual({ screen: "file", rootId: root.root_id, path: "docs/answers.md", taskId: asked.task_id });
    expect(external).toHaveBeenCalledTimes(1);
    // Markdown's URL filter leaves unsafe links inert; no permission is
    // granted and neither external navigation nor file navigation starts work.
    fireEvent.click(reply.getByText("Unsafe link"));
    expect(external).toHaveBeenCalledTimes(1);
    expect(app.session.state.get().grants).toEqual(grants);
    expect(Object.values(app.session.commands.get())).toEqual([]);
  });

  it("does not show pre-answer, equal-time or other-run text, or cross the next question", async () => {
    open(entry);
    append({ kind: "agent_message", text: "Before the person answered", partial: false });
    await advance();
    settle();
    const at = Date.now();
    append({ kind: "agent_message", text: "Ambiguous equal-time text", partial: false }, at);
    append({ kind: "agent_message", text: "Different run's response", partial: false }, at + 1, ids.rHeaders);
    append({ kind: "agent_message", text: explanation, partial: false }, at + 2);
    const next: Approval = { ...asked, approval_id: "next-question", requested_at: at + 3 };
    act(() => window.__sim!.host.emit({ type: "approval_upserted", approval: next }));
    append({ kind: "approval", approval_id: next.approval_id }, at + 3);
    append({ kind: "agent_message", text: "For the next question", partial: false }, at + 4);
    await advance();
    const reply = within(screen.getByRole("region", { name: "Agent’s reply" }));
    expect(reply.getByText(explanation)).toBeTruthy();
    for (const text of ["Before the person answered", "Ambiguous equal-time text", "Different run's response", "For the next question"]) expect(reply.queryByText(text)).toBeNull();
    expect(reply.getByText(/Some messages can’t be safely matched/)).toBeTruthy();
  });

  it.each(["person message", "run end", "new run"])("ends the reply at the next %s", async boundary => {
    open(entry);
    settle();
    const at = Date.now();
    append({ kind: "agent_message", text: explanation, partial: false }, at + 1);
    append(boundary === "person message" ? { kind: "user_message", text: "A different topic", bundle: null, delivery: "native_delivered" }
      : boundary === "run end" ? { kind: "run_ended", run_id: asked.run_id, state: "completed" }
      : { kind: "run_started", run_id: ids.rHeaders }, at + 2);
    append({ kind: "agent_message", text: "Unrelated later reply", partial: false }, at + 3);
    await advance();
    const reply = within(screen.getByRole("region", { name: "Agent’s reply" }));
    expect(reply.getByText(explanation)).toBeTruthy();
    expect(reply.queryByText("Unrelated later reply")).toBeNull();
  });

  it("retains structured question prose without old option controls burying its reply", async () => {
    const prompt = "Should existing records be preserved or converted?";
    asked = { ...asked, questions: [{ id: "approach", question: prompt, options: [{ label: "Preserve", description: "Keep old records" }, { label: "Convert", description: "Rewrite old records" }] }] };
    act(() => window.__sim!.host.emit({ type: "approval_upserted", approval: asked }));
    await advance();
    open(entry);
    settle();
    append({ kind: "agent_message", text: explanation, partial: false }, Date.now() + 1);
    await advance();
    expect(screen.getByText(prompt)).toBeTruthy();
    expect(screen.queryByRole("radio")).toBeNull();
    expect(screen.queryByRole("checkbox")).toBeNull();
    expect(screen.queryByText("Keep old records")).toBeNull();
    expect(screen.queryByText("Rewrite old records")).toBeNull();
    expect(screen.getByText(explanation)).toBeTruthy();
  });

  it("preserves the typed answer and error when delivery fails, without showing an agent reply", async () => {
    open(entry);
    window.__sim!.host.failNextApprovalDelivery = true;
    await sendAnswer();
    append({ kind: "agent_message", text: "Unrelated explanation after failed delivery", partial: false });
    await advance();
    expect(app.session.state.get().approvals[asked.approval_id]?.state).toBe("pending");
    expect(screen.getByRole("textbox", { name: "Your answer" })).toHaveProperty("value", answer);
    expect(screen.getAllByText(/The agent did not receive this/).length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Send answer" })).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Agent’s reply" })).toBeNull();
  });

  it("shows cached explanation offline without pretending it is current", async () => {
    open(entry);
    settle();
    append({ kind: "agent_message", text: explanation, partial: false }, Date.now() + 1);
    await advance();
    act(() => window.__sim!.offline());
    await advance();
    expect(screen.getByText(explanation)).toBeTruthy();
    expect(screen.getByText(/Offline. Showing saved replies only/)).toBeTruthy();
    expect(screen.queryByText("Waiting for agent’s reply…")).toBeNull();
  });

  it("does not fetch or reveal replies without task-read permission, even when cached", async () => {
    open(entry);
    settle();
    await advance();
    append({ kind: "agent_message", text: explanation, partial: false });
    await advance();
    expect(screen.getByText(explanation)).toBeTruthy();
    act(() => app.session.state.set(s => ({ ...s, grants: { ...s.grants!, tasks: false } })));
    const load = vi.spyOn(app.session, "loadTimeline");
    expect(screen.getByText("This device was not granted permission to read task replies.")).toBeTruthy();
    expect(screen.queryByText(explanation)).toBeNull();
    expect(screen.queryByRole("button", { name: "Open task" })).toBeNull();
    expect(load).not.toHaveBeenCalled();
    expect(app.session.state.get().timelineReaders[asked.task_id] ?? 0).toBe(0);
  });

  it("does not add reply clutter or fetch history for ordinary permissions", async () => {
    const permission = { ...asked, kind: "command" as const };
    act(() => window.__sim!.host.emit({ type: "approval_upserted", approval: permission }));
    await advance();
    const load = vi.spyOn(app.session, "loadTimeline");
    open(entry);
    expect(screen.getAllByRole("button", { name: "Allow" }).length).toBeGreaterThan(0);
    settle({ kind: "command", delivery: { decision: { decision: "allow" }, at: Date.now(), error: null } });
    await advance();
    expect(screen.queryByRole("region", { name: "Agent’s reply" })).toBeNull();
    expect(load).not.toHaveBeenCalled();
    expect(Object.keys(app.session.commands.get())).toHaveLength(0);
  });

  it("reports a remotely settled question with no delivered answer honestly", async () => {
    open(entry);
    const load = vi.spyOn(app.session, "loadTimeline");
    settle({ state: "resolved_elsewhere", delivery: undefined });
    await advance();
    expect(screen.getByText(asked.title)).toBeTruthy();
    expect(screen.getByText(/hasn’t recorded a delivered answer/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Send answer" })).toBeNull();
    expect(load).not.toHaveBeenCalled();
  });

  it("automatically loads older history until this question is found, preserving its earlier explanation", async () => {
    open(entry);
    const at = Date.now();
    history.push({ seq: 10001, at: at + 1, run_id: asked.run_id, item: { kind: "agent_message", text: explanation, partial: false } });
    for (let i = 2; i <= 165; i++) history.push({ seq: 10000 + i, at: at + i, run_id: asked.run_id, item: { kind: "activity", tool: "Read", summary: "Read a file", status: "succeeded", detail: null } });
    history.push({ seq: 10166, at: at + 166, run_id: asked.run_id, item: { kind: "agent_message", text: final, partial: false } });
    const load = vi.spyOn(app.session, "loadTimeline");
    settle();
    await advance(2000);
    expect(load.mock.calls.filter(([, older]) => older)).toHaveLength(2);
    expect(screen.getByText(explanation)).toBeTruthy();
    expect(screen.getByText(final)).toBeTruthy();
  });

  it("keeps missing history and history failures distinct from waiting for a reply", async () => {
    open(entry);
    history = [{ seq: 10001, at: Date.now() + 1, run_id: asked.run_id, item: { kind: "agent_message", text: "Cannot attribute without the question", partial: false } }];
    settle();
    await advance();
    expect(screen.getByText(/question isn’t in the available conversation history/)).toBeTruthy();
    expect(screen.queryByText("Cannot attribute without the question")).toBeNull();
    const load = vi.spyOn(app.session, "loadTimeline").mockRejectedValue(new Error("History unavailable"));
    act(() => window.__sim!.offline());
    await advance();
    expect(screen.getByText(/Offline. Showing saved replies only/)).toBeTruthy();
    expect(screen.queryByText("Waiting for agent’s reply…")).toBeNull();
    act(() => window.__sim!.online());
    await advance(3000);
    expect(screen.getByText(/Couldn’t load the reply: History unavailable/)).toBeTruthy();
    expect(load).toHaveBeenCalled();
    load.mockRestore();
    history.unshift({ seq: 10000, at: asked.requested_at, run_id: asked.run_id, item: { kind: "approval", approval_id: asked.approval_id } });
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await advance();
    expect(screen.getByText("Cannot attribute without the question")).toBeTruthy();
  });
});
