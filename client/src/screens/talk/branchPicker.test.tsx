// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { createTaskCommand } from "./intent";
import Review from "./Review";
import { TaskAgentSheet } from "./TaskAgent";

let app: App;
const astra = { harness: "codex" as const, model: "gpt-6-astra", effort: "high" };
const tick = async () => act(async () => { await vi.advanceTimersByTimeAsync(900); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick();
  // The project's writer would hold a new Work task in Next up.
  const host = window.__sim!.host;
  const project = { ...session.state.get().projects[ids.pTide]!, active_writer: null };
  host.projects = host.projects.map(p => p.project_id === project.project_id ? project : p);
  host.emit({ type: "project_upserted", project });
  // These are about branches alone: a GitHub link would choose the branch.
  host.github.settings.delete(ids.pTide);
  await tick();
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });
function view(route: Route) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}><Review route={route as Extract<Route, { screen: "review" }>} /></RouteProvider></AppProvider>);
}
const created = () => Object.values(app.session.commands.get()).find(c => c.body.command === "create_task")?.body;

it("names no branch by default", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Check the parser", agent: astra, mode: "work" });
  view({ screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" }); await tick();
  expect(screen.getByRole("button", { name: "Branch: All branches" })).toBeTruthy();
  expect(screen.getByText("fix/retry-flake is checked out · 3 uncommitted changes.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).toMatchObject({ command: "create_task", project_id: ids.pTide });
  expect(created()).not.toHaveProperty("branch");
  const task = Object.values(app.session.state.get().tasks).find(t => t.title === "Check the parser")!;
  expect(task.branch).toBeUndefined();
});

it("lists the computer's branches and tells the agent the one chosen", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Add rate limits", agent: astra, mode: "work" });
  view({ screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" }); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Branch: All branches" })); await tick();
  const sheet = screen.getByRole("radiogroup", { name: "Branch" });
  expect(within(sheet).getByRole("radio", { name: /All branches/ }).getAttribute("aria-checked")).toBe("true");
  expect(within(sheet).getByRole("radio", { name: /fix\/retry-flake.*Checked out now · 3 uncommitted changes/ })).toBeTruthy();
  // Thirteen branches: long enough to find one by name.
  fireEvent.change(screen.getByLabelText("Find a branch"), { target: { value: "rate" } });
  expect(within(sheet).getAllByRole("radio").map(r => r.textContent)).toEqual(["feature/rate-limits"]);
  fireEvent.click(within(sheet).getByRole("radio", { name: "feature/rate-limits" }));
  expect(screen.getByRole("button", { name: "Branch: feature/rate-limits" })).toBeTruthy();
  expect(screen.getByText(/fix\/retry-flake is checked out · 3 uncommitted changes\. The agent is asked to keep them if it switches\./)).toBeTruthy();
  // The choice is kept with the draft.
  expect(app.ui.draft(ids.pTide, null)?.review?.branch).toEqual({ name: "feature/rate-limits" });
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).toMatchObject({ command: "create_task", branch: { name: "feature/rate-limits" } });
  expect(created()).not.toHaveProperty("branch.new");
  const task = Object.values(app.session.state.get().tasks).find(t => t.title === "Add rate limits")!;
  expect(task.branch).toEqual({ name: "feature/rate-limits" });
});

it("starts a Next up item on its source task's branch, which can be cleared", async () => {
  const host = window.__sim!.host;
  // The source task made it, so Next up works on it as it stands.
  const source = { ...app.session.state.get().tasks[ids.tHeaders]!, branch: { name: "feature/webhook-retries", new: true } };
  host.tasks = host.tasks.map(t => t.task_id === source.task_id ? source : t);
  host.emit({ type: "task_upserted", task: source });
  app.ui.saveDraft(source.project_id, source.task_id, { text: "Then tidy the headers" });
  view({ screen: "review", projectId: source.project_id, taskId: source.task_id, intent: "next_up" }); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Branch: feature/webhook-retries" })); await tick();
  expect(screen.getByRole("radio", { name: "feature/webhook-retries" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(screen.getByRole("radio", { name: /All branches/ }));
  expect(screen.getByRole("button", { name: "Branch: All branches" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Add to Next up" })); await tick();
  expect(created()).toMatchObject({ command: "create_task", placement: { mode: "queue" } });
  expect(created()).not.toHaveProperty("branch");
});

it("shows a task's branch where its agent and flags are", () => {
  const agent = { label: "Codex", status: "", flags: "Ask for approval", risk: null, launch: { harness: "codex" as const, policy: "standard" as const, mode: "work" as const }, branch: { name: "feature/search" }, started: null };
  const { rerender } = render(<AppProvider app={app}><TaskAgentSheet open onClose={() => {}} agent={agent} /></AppProvider>);
  expect(screen.getByText("The agent is told to work on feature/search.")).toBeTruthy();
  rerender(<AppProvider app={app}><TaskAgentSheet open onClose={() => {}} agent={{ ...agent, branch: { name: "feature/search", new: true } }} /></AppProvider>);
  expect(screen.getByText("The agent is told to make feature/search and work there.")).toBeTruthy();
});

it("names a new branch for the agent to make and work on", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Add rate limits", agent: astra, mode: "work" });
  view({ screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" }); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Branch: All branches" })); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^New branch/ }));
  const field = screen.getByLabelText<HTMLInputElement>("Branch name");
  // Typed on a phone: Git allows no spaces, so they become hyphens.
  fireEvent.change(field, { target: { value: "rate limits v2" } });
  expect(field.value).toBe("rate-limits-v2");
  expect(screen.getByText("The agent makes it from fix/retry-flake before changing any file, and your 3 uncommitted changes come with it.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Use new branch" }));
  expect(screen.getByRole("button", { name: "Branch: rate-limits-v2, new" })).toBeTruthy();
  expect(screen.getByText("New branch from fix/retry-flake, bringing 3 uncommitted changes. The agent makes it before changing any file.")).toBeTruthy();
  expect(app.ui.draft(ids.pTide, null)?.review?.branch).toEqual({ name: "rate-limits-v2", new: true });
  // Reopened, the sheet shows it chosen, and the form starts from its name.
  fireEvent.click(screen.getByRole("button", { name: "Branch: rate-limits-v2, new" })); await tick();
  expect(screen.getByRole("radio", { name: /^rate-limits-v2/ }).getAttribute("aria-checked")).toBe("true");
  expect(screen.getByRole("radio", { name: /All branches/ }).getAttribute("aria-checked")).toBe("false");
  fireEvent.click(screen.getByRole("button", { name: /^New branch/ }));
  expect(screen.getByLabelText<HTMLInputElement>("Branch name").value).toBe("rate-limits-v2");
  fireEvent.click(screen.getByRole("button", { name: "Back to the list" }));
  fireEvent.click(screen.getByRole("radio", { name: /^rate-limits-v2/ }));
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).toMatchObject({ command: "create_task", branch: { name: "rate-limits-v2", new: true } });
  const task = Object.values(app.session.state.get().tasks).find(t => t.title === "Add rate limits")!;
  expect(task.branch).toEqual({ name: "rate-limits-v2", new: true });
});

it("says why a new branch's name won't do before it can be used", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Add rate limits", agent: astra, mode: "work" });
  view({ screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" }); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Branch: All branches" })); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^New branch/ }));
  const field = screen.getByLabelText("Branch name");
  const use = () => screen.getByRole<HTMLButtonElement>("button", { name: "Use new branch" });
  expect(use().disabled).toBe(true);
  expect(screen.queryByRole("alert")).toBeNull();
  fireEvent.change(field, { target: { value: "feature/.hidden" } });
  expect(screen.getByRole("alert").textContent).toBe("No part of a branch name can start with “.”.");
  expect(field.getAttribute("aria-invalid")).toBe("true");
  expect(use().disabled).toBe(true);
  fireEvent.change(field, { target: { value: "main" } });
  expect(screen.getByRole("alert").textContent).toBe("main already exists. Go back to choose it from the list.");
  expect(use().disabled).toBe(true);
  // Partway through a name, it waits until the field is left.
  fireEvent.change(field, { target: { value: "feature/" } });
  expect(screen.queryByRole("alert")).toBeNull();
  expect(use().disabled).toBe(true);
  fireEvent.blur(field);
  expect(screen.getByRole("alert").textContent).toBe("A branch name can’t start or end with “/”, or have “//”.");
  fireEvent.change(field, { target: { value: "feature/search" } });
  expect(screen.queryByRole("alert")).toBeNull();
  expect(use().disabled).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "Back to the list" }));
  expect(screen.getByRole("radio", { name: /All branches/ }).getAttribute("aria-checked")).toBe("true");
});

it("offers a search that matches no branch as a new one", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Try a spike", agent: astra, mode: "work" });
  view({ screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" }); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Branch: All branches" })); await tick();
  const find = screen.getByLabelText("Find a branch");
  // An exact match is offered as itself, not again as new.
  fireEvent.change(find, { target: { value: "release/1.4" } });
  expect(screen.queryByRole("button", { name: /^New branch/ })).toBeNull();
  fireEvent.change(find, { target: { value: "spike/" } });
  expect(screen.queryByRole("button", { name: /^New branch/ })).toBeNull();
  fireEvent.change(find, { target: { value: "spike/new idea" } });
  expect(screen.getByText("No branch matches “spike/new idea”.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^New branch “spike\/new-idea”/ }));
  expect(screen.getByRole("button", { name: "Branch: spike/new-idea, new" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).toMatchObject({ command: "create_task", branch: { name: "spike/new-idea", new: true } });
});

it("won't name a branch to a companion that can't pass it on", async () => {
  const caps = app.session.state.get().capabilities!;
  app.session.state.set(s => ({ ...s, capabilities: { ...caps, features: caps.features.filter(f => f !== "task_branch_v1") } }));
  app.ui.saveDraft(ids.pTide, null, { text: "Check the parser", agent: astra, mode: "work" });
  view({ screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" }); await tick();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Branch: All branches" }).disabled).toBe(true);
  expect(screen.getByText(/Update Øphio on this computer to choose a branch/)).toBeTruthy();
  const body = createTaskCommand({ projectId: ids.pTide, prompt: "Inspect parser", title: null, agent: astra, mode: "work", placement: { mode: "start_now" }, mission: null, bundle: null, branch: { name: "main" } });
  const before = app.session.commands.get();
  await expect(app.session.command(body)).rejects.toThrow(/choose a branch/);
  expect(app.session.commands.get()).toEqual(before);
});
