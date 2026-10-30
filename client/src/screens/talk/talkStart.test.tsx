// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { Transcript } from "./Transcript";
import { Composer } from "./Composer";
import { AgentSheet } from "./AgentSheet";
import { annotationStore } from "../../state/annotations";
import { Toasts } from "../../design/Toast";
import Review from "./Review";

let app: App;
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await vi.advanceTimersByTimeAsync(900);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks(); });
const tick = async (ms = 250) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };
function view(child: React.ReactNode, route: Route = { screen: "talk" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{child}</RouteProvider></AppProvider>);
}
function freeWriter() {
  const host = window.__sim!.host;
  const project = host.projects.find((p) => p.project_id === ids.pTide)!;
  const free = { ...project, active_writer: null };
  host.projects = host.projects.map((p) => p.project_id === free.project_id ? free : p);
  host.emit({ type: "project_upserted", project: free });
}

it("prefills the first markup comment once and places a refusal by Start with focus on the editable request", async () => {
  freeWriter();
  const store = annotationStore(app.profile.profileId);
  const annotation = store.create(ids.pTide, null);
  store.update(annotation.id, d => ({ ...d, comments: [{ comment_id: "comment", text: "Move the chart title above the legend", mark_ids: [], origin: { kind: "typed" }, link_origins: [] }] }));
  app.ui.saveDraft(ids.pTide, null, { text: "", annotationId: annotation.id });
  const route = { screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" } as const;
  const { container } = view(<Review route={route} />, route);
  await tick();
  const field = screen.getByRole<HTMLTextAreaElement>("textbox", { name: "What you’re asking" });
  expect(field.value).toBe("Move the chart title above the legend");
  expect(screen.getByText(/From your first markup comment/)).toBeTruthy();
  fireEvent.change(field, { target: { value: "" } });
  fireEvent.click(screen.getByRole("button", { name: "Start task" }));
  expect(document.activeElement).toBe(field);
  expect(within(container.querySelector(".review-foot") as HTMLElement).getByRole("alert").textContent).toContain("Say what you want done");
  expect(field.value).toBe("");
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "create_task")).toBe(false);
});

it("shows every markup comment, and the marks each is about, without opening the editor", async () => {
  freeWriter();
  const store = annotationStore(app.profile.profileId);
  const annotation = store.create(ids.pTide, null);
  const mark = (mark_id: string, label: string) => ({ mark_id, label, source_key: "S1", shape: { kind: "arrow" as const, from: { x: 0, y: 0, t: 0 }, to: { x: 1, y: 1, t: 0 } }, color: 0, created_at: 0, deleted: false });
  const comment = (comment_id: string, text: string, mark_ids: string[]) => ({ comment_id, text, mark_ids, origin: { kind: "typed" as const }, link_origins: mark_ids.map(() => "manual" as const) });
  store.update(annotation.id, d => ({
    ...d,
    sources: [{ key: "S1", blobKey: "blob", artifactId: null, sha256: null, mediaType: "image/png", width: 10, height: 10, durationMs: null, origin: { kind: "screenshot" }, capturedAt: 0, derivedFrom: null, derivation: null } as never],
    marks: [mark("m-a", "A"), mark("m-b", "B")],
    comments: [comment("c1", "The bill controls need clearer mobile alignment.", ["m-a"]), comment("c2", "Total wraps under the tip", ["m-a", "m-b"]), comment("c3", "Keep the dark theme", []), comment("c4", "  ", [])],
  }));
  app.ui.saveDraft(ids.pTide, null, { text: "", annotationId: annotation.id });
  const route = { screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" } as const;
  view(<Review route={route} />, route);
  await tick();
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "What you’re asking" }).value).toBe("The bill controls need clearer mobile alignment.");
  expect(screen.getByText(/From your first markup comment; all 3 go with the markup/)).toBeTruthy();
  const listed = within(screen.getByRole("list", { name: "Markup comments" })).getAllByRole("listitem").map(item => item.textContent);
  expect(listed).toEqual(["A The bill controls need clearer mobile alignment.", "A, B Total wraps under the tip", "General Keep the dark theme"]);
  expect(screen.getByRole("button", { name: /^Markup.*Edit$/ })).toBeTruthy();
  expect(app.ui.get().stacks.talk.some(r => r.screen === "annotate")).toBe(false);
});

it("retains each agent and model effort through a round trip and an applied sheet", async () => {
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const project = app.session.state.get().projects[ids.pTide]!;
  const sheet = () => <AgentSheet open onClose={() => {}} project={project} value={{ harness: "claude_code", model: "claude-opus-5-5", effort: "high" }} onChange={() => {}} />;
  view(sheet());
  fireEvent.click(screen.getByRole("button", { name: /^Codex/ }));
  // One chip says default: Agent’s default, not also the model it is.
  const models = within(screen.getByRole("group", { name: "Model" })).getAllByRole("button").map((b) => b.textContent);
  expect(models.filter((m) => /default/i.test(m ?? ""))).toEqual(["Agent’s default"]);
  fireEvent.click(screen.getByRole("button", { name: "GPT-6-Sol" }));
  fireEvent.click(screen.getByRole("button", { name: "Medium" }));
  fireEvent.click(screen.getByRole("button", { name: /GPT-6-Astra/ }));
  fireEvent.click(screen.getByRole("button", { name: "High" }));
  fireEvent.click(screen.getByRole("button", { name: /^Claude Code/ }));
  fireEvent.click(screen.getByRole("button", { name: /^Codex/ }));
  fireEvent.click(screen.getByRole("button", { name: "GPT-6-Sol" }));
  expect(screen.getByRole("button", { name: "Medium" }).getAttribute("aria-pressed")).toBe("true");
  expect(screen.getByText(/Effort changed to Medium.*restoring your choice/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Claude Code/ }));
  fireEvent.click(screen.getByRole("button", { name: "Use this agent" }));
  cleanup();
  view(sheet());
  fireEvent.click(screen.getByRole("button", { name: /^Codex/ }));
  expect(screen.getByRole("button", { name: "GPT-6-Sol" }).getAttribute("aria-pressed")).toBe("true");
  expect(screen.getByRole("button", { name: "Medium" }).getAttribute("aria-pressed")).toBe("true");
});

it("labels a new task from a finished Codex conversation with its receiving agent", async () => {
  const state = app.session.state.get();
  const task = { ...state.tasks[ids.tHeaders]!, harness: "codex" as const };
  const project = state.projects[task.project_id]!;
  freeWriter();
  view(<Composer project={project} task={task} run={state.runs[ids.rHeaders]!} harness={state.harnesses.find(h => h.kind === "codex")!} agent={null} />);
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  fireEvent.click(screen.getByRole("radio", { name: /Start a new task/ }));
  expect(screen.getByRole("textbox", { name: "What should Claude Code do in tidewater-api?" })).toBeTruthy();
});

it("lists the ways to send as radios that say which one is chosen", async () => {
  const state = app.session.state.get();
  const task = { ...state.tasks[ids.tHeaders]!, harness: "codex" as const };
  freeWriter();
  view(<Composer project={state.projects[task.project_id]!} task={task} run={state.runs[ids.rHeaders]!} harness={state.harnesses.find(h => h.kind === "codex")!} agent={null} />);
  fireEvent.click(screen.getByRole("button", { name: "Continue this task" }));
  const group = screen.getByRole("radiogroup", { name: "How to send" });
  const checked = () => within(screen.getByRole("radiogroup", { name: "How to send" })).getAllByRole("radio").filter(r => r.getAttribute("aria-checked") === "true").map(r => r.textContent);
  expect(within(group).getAllByRole("radio")).toHaveLength(2);
  expect(checked()).toEqual([expect.stringMatching(/^Continue this task/)]);
  fireEvent.click(within(group).getByRole("radio", { name: /^Start a new task/ }));
  fireEvent.click(screen.getByRole("button", { name: "Start a new task" }));
  expect(checked()).toEqual([expect.stringMatching(/^Start a new task/)]);
});

it("announces the project switch when starting a task for another project", async () => {
  app.ui.selectProject(ids.pTide);
  app.ui.saveDraft(ids.pNotes, null, { text: "Organize these notes" });
  const route = { screen: "review", projectId: ids.pNotes, taskId: null, intent: "new_task" } as const;
  app.ui.push(route, "talk");
  view(<><Review route={route} /><Toasts /></>, route);
  expect(screen.getByText("Starting this task switches to field-notes.")).toBeTruthy();
  await tick(800);
  fireEvent.click(screen.getByRole("button", { name: "Start task" }));
  await tick(800);
  expect(app.ui.get().projectId).toBe(ids.pNotes);
  expect(screen.getByText("Switched to field-notes for this task")).toBeTruthy();
});

it("makes a re-presented inline approval actionable after an earlier accepted decision", async () => {
  const state = app.session.state.get();
  const approval = state.approvals[ids.aTest]!;
  view(<Transcript task={state.tasks[ids.tFlaky]!} agent="Claude Code" />);
  await tick();
  fireEvent.click(screen.getByRole("button", { name: "Allow" }));
  await tick();
  act(() => window.__sim!.host.emit({ type: "approval_upserted", approval: { ...approval, requested_at: approval.requested_at + 1 } }));
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Allow" }).disabled).toBe(false);
  expect(screen.queryByText(/Decision accepted/)).toBeNull();
});
