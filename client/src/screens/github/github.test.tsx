// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, freshReview, type Route } from "../../app/ui";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { SIM_DESKTOP_PASSWORD } from "../../dev/sim/host";
import { Toasts } from "../../design/Toast";
import { HostSession } from "../../host/session";
import type { Command, Project, Task } from "../../protocol/types";
import { RouteProvider } from "../../shell/Screen";
import { Composer } from "../talk/Composer";
import Review from "../talk/Review";
import Work from "../work/Work";
import { NewWork } from "../work/NewWork";
import ProjectSheet from "../work/ProjectSheet";
import Github from "./Github";
import { GithubDoors } from "./GithubDoors";
import GithubList, { lineWords, togetherWords } from "./GithubList";
import GithubNew from "./GithubNew";
import { PrChip, PrSheet } from "./PrSheet";

let app: App;
let writer: Project["active_writer"];
const astra = { harness: "codex" as const, model: "gpt-6-astra", effort: "high" };
// The sim's Codex can't promise Plan changes nothing, so read-only work goes to Claude Code.
const opus = { harness: "claude_code" as const, model: "claude-opus-5-5", effort: null };
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
  writer = session.state.get().projects[ids.pTide]!.active_writer ?? null;
  const project = { ...session.state.get().projects[ids.pTide]!, active_writer: null };
  host.projects = host.projects.map(p => p.project_id === project.project_id ? project : p);
  host.emit({ type: "project_upserted", project });
  await tick();
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

function view(node: ReactNode, route: Route = { screen: "talk" }) {
  return render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", depth: 1, active: true, route }}>{node}<Toasts /></RouteProvider></AppProvider>);
}
function review() {
  const route = { screen: "review", projectId: ids.pTide, taskId: null, intent: "new_task" } as const;
  return view(<Review route={route} />, route);
}
const sent = (): Command[] => Object.values(app.session.commands.get()).map(c => c.body);
const created = () => sent().find(c => c.command === "create_task");
const github = () => sent().flatMap(c => (c.command === "github" ? [c.action] : []));
const taskTitled = (title: string) => Object.values(app.session.state.get().tasks).find(t => t.title === title)!;
const chooseLink = async (name: RegExp) => {
  fireEvent.click(screen.getByRole("button", { name: /^GitHub: / })); await tick();
  fireEvent.click(within(screen.getByRole("radiogroup", { name: "What it’s for on GitHub" })).getByRole("radio", { name })); await tick();
};

it("links new Work to a draft pull request while the project's workflow is on", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Add a retry budget header", agent: astra, mode: "work" });
  review(); await tick();
  expect(screen.getByRole("button", { name: "GitHub: New pull request" })).toBeTruthy();
  expect(screen.getByText("You mark it ready and merge it.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Branch: A new branch" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).toMatchObject({ project_id: ids.pTide, github: { type: "new_pr" } });
  expect(created()).not.toHaveProperty("branch");
  expect(taskTitled("Add a retry budget header").github?.link).toEqual({ type: "new_pr" });
});

it("leaves Ask and Plan unlinked, and a project with the workflow off shows no GitHub row", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Explain the limiter", agent: astra, mode: "ask" });
  const mounted = review(); await tick();
  expect(screen.getByRole("button", { name: "GitHub: Not linked" })).toBeTruthy();
  mounted.unmount();
  window.__sim!.host.github.settings.delete(ids.pTide);
  app.ui.saveDraft(ids.pTide, null, { mode: "work" });
  review(); await tick();
  expect(screen.queryByRole("button", { name: /^GitHub: / })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).not.toHaveProperty("github");
});

it("takes over a pull request on its own branch", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Answer the question about Reset", agent: astra, mode: "work" });
  review(); await tick();
  await chooseLink(/^Take over a pull request/);
  fireEvent.change(screen.getByLabelText("The pull request’s number or link"), { target: { value: "https://github.com/someone/else/pull/214" } });
  expect(screen.getByText(/Type #12, 12, or a link to a pull request in samrivera\/tidewater-api/)).toBeTruthy();
  fireEvent.change(screen.getByLabelText("The pull request’s number or link"), { target: { value: "#214" } });
  fireEvent.click(screen.getByRole("button", { name: "Use #214" })); await tick();
  expect(screen.getByRole("button", { name: "GitHub: Pull request #214" })).toBeTruthy();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Branch: The pull request’s own" }).disabled).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).toMatchObject({ github: { type: "pr", number: 214 } });
  expect(created()).not.toHaveProperty("branch");
  // The task takes the pull request's title and its branch.
  const task = Object.values(app.session.state.get().tasks).find(t => t.task_id !== ids.tHeaders && t.github?.link?.type === "pr")!;
  expect(task).toMatchObject({ title: "Add rate-limit headers to /v1/orders", branch: { name: "ophio/rate-limit-headers" } });
});

it("keeps sorting issues in Plan, and won't start it in Work", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Sort the backlog", agent: opus, mode: "work" });
  review(); await tick();
  await chooseLink(/^Sort issues/);
  expect(screen.getByRole("button", { name: "GitHub: Sort issues" })).toBeTruthy();
  expect(screen.getByRole("heading", { name: "Sort with" })).toBeTruthy();
  expect(screen.getByRole<HTMLInputElement>("radio", { name: "Plan" }).checked).toBe(true);
  fireEvent.click(screen.getByRole("radio", { name: /^Work/ })); await tick();
  expect(screen.getByText("Sorting issues only reads, so it runs in Plan. Choose Plan, or another GitHub link.")).toBeTruthy();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Start task" }).disabled).toBe(true);
  fireEvent.click(screen.getByRole("radio", { name: "Plan" })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).toMatchObject({ mode: "plan", github: { type: "triage" } });
  expect(taskTitled("Sort the backlog").mode).toBe("plan");
});

it("merges the commit it showed, behind the phone's unlock", async () => {
  const confirm = vi.spyOn(app.platform.appLock!, "confirm");
  view(<PrChip task={app.session.state.get().tasks[ids.tHeaders]!} />);
  fireEvent.click(screen.getByRole("button", { name: "PR #214 · Ready · Checks pass · 1 open thread" })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "#214 Add rate-limit headers to /v1/orders" }));
  expect(sheet.getByText("Made by GPT-6-Astra")).toBeTruthy();
  expect(sheet.getByText("src/orders/limits.rs:42")).toBeTruthy();
  expect(sheet.getByText("Merges commit 5b7e2c9 into main, and nothing newer. Your phone’s unlock confirms it.")).toBeTruthy();
  expect(sheet.queryByRole("button", { name: "Fix failing checks" })).toBeNull();

  // `origin` moved to another repository since the sheet was read stops the merge.
  const repo = window.__sim!.host.github.repo;
  repo.name = "tidewater-web";
  fireEvent.click(sheet.getByRole("button", { name: "Squash and merge" })); await tick();
  expect(github().at(-1)).toMatchObject({ action: "merge_pr", repo: "samrivera/tidewater-api", number: 214 });
  expect(screen.getByText(/is samrivera\/tidewater-web now, not the repository whose pull request you read/)).toBeTruthy();
  expect(window.__sim!.host.github.prs.get(214)!.summary.state).toBe("open");
  repo.name = "tidewater-api"; await tick();

  // An unlock turned down sends nothing new.
  const before = github().length;
  confirm.mockRejectedValueOnce(new Error("Not unlocked. Try again with your screen lock."));
  fireEvent.click(sheet.getByRole("button", { name: "Squash and merge" })); await tick();
  expect(screen.getByText("Not unlocked. Try again with your screen lock.")).toBeTruthy();
  expect(github()).toHaveLength(before);

  // A commit pushed since the sheet was read stops the merge.
  const pr = window.__sim!.host.github.prs.get(214)!;
  const shown = pr.summary.head;
  pr.summary.head = "c0ffee0000000000000000000000000000000000";
  fireEvent.click(sheet.getByRole("button", { name: "Squash and merge" })); await tick();
  expect(confirm).toHaveBeenLastCalledWith("Merge pull request #214");
  expect(github().at(-1)).toMatchObject({ action: "merge_pr", repo: "samrivera/tidewater-api", number: 214, expected_head: shown, method: "squash" });
  expect(screen.getByText(/has a new commit, c0ffee0, since you looked/)).toBeTruthy();
  expect(pr.summary.state).toBe("open");

  pr.summary.head = shown;
  fireEvent.click(sheet.getByRole("button", { name: "Squash and merge" })); await tick();
  expect(screen.getByText("Pull request #214 merged")).toBeTruthy();
  expect(app.session.state.get().tasks[ids.tHeaders]!.github?.pr?.state).toBe("merged");
});

it("marks a draft ready behind the unlock, and drafts a fix for its failing checks in Review", async () => {
  const confirm = vi.spyOn(app.platform.appLock!, "confirm");
  const task = app.session.state.get().tasks[ids.tHeaders]!;
  view(<PrSheet open onClose={() => {}} projectId={ids.pTide} number={217} task={task} />); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "#217 Page the orders export" }));
  expect(sheet.queryByRole("button", { name: "Address comments" })).toBeNull();
  fireEvent.click(sheet.getByRole("button", { name: "Mark ready" })); await tick();
  expect(confirm).toHaveBeenLastCalledWith("Mark pull request #217 ready");
  expect(github().at(-1)).toEqual({ action: "mark_pr_ready", project_id: ids.pTide, repo: "samrivera/tidewater-api", number: 217 });
  expect(screen.getByText("Pull request #217 is ready for review")).toBeTruthy();
  expect(sheet.getByRole<HTMLButtonElement>("button", { name: "Squash and merge" }).disabled).toBe(true);
  expect(sheet.getByText("The required check test is failing.")).toBeTruthy();

  fireEvent.click(sheet.getByRole("button", { name: "Fix failing checks" })); await tick();
  expect(app.ui.draft(ids.pTide, null)).toMatchObject({
    text: "Fix the failing checks on pull request #217.", mode: "work",
    agent: { harness: task.harness, model: task.model ?? null, effort: task.effort ?? null },
    review: { github: { type: "pr", number: 217 } },
  });
  expect(app.ui.get().stacks.talk.at(-1)).toMatchObject({ screen: "review", projectId: ids.pTide, intent: "new_task" });
});

it("says starting a Find resumes a paused Next up, so its fixes can start", async () => {
  const host = window.__sim!.host;
  host.queue = { ...host.queue, paused: true, pause_reason: "Next up is paused. Resume it to let tasks start.", revision: host.queue.revision + 1 };
  host.emit({ type: "queue_changed", queue: host.queue });
  app.ui.saveDraft(ids.pTide, null, { text: "Find issues in the project", mode: "plan", agent: opus, review: { title: "", stopWhen: "", placement: null, authorization: "ask_before_start", github: { type: "find", scope: { scope: "project" }, fix_with: opus } } });
  review(); await tick();
  expect(screen.getByText(/Each issue it files becomes/).textContent).toBe("It files up to 5. Each issue it files becomes “Fix #N” in Next up with this agent. They start one after another by themselves. Next up is paused, so starting this Find resumes it.");
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).toMatchObject({ mode: "plan", github: { type: "find" }, placement: { mode: "start_now" } });
  expect(app.session.state.get().queues[ids.pTide]?.paused).toBe(false);
});

it("starts a Find's fixes with this phone's launch defaults, or the flags chosen for them", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Find issues in the project", mode: "plan", agent: opus, review: { title: "", stopWhen: "", placement: null, authorization: "ask_before_start", github: { type: "find", scope: { scope: "project" }, fix_with: opus } } });
  review(); await tick();
  const fix = within(screen.getByText("Fix with").closest("section")!);
  expect(fix.getByText("Permission prompts: Skip permissions")).toBeTruthy();
  expect(fix.getByRole("note").textContent).toMatch(/won’t ask before running commands or changing files/);
  expect(fix.getByText("--dangerously-skip-permissions").closest("details")).toBeTruthy();
  fireEvent.click(fix.getByRole("button", { name: /Claude Code/ })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "Agent for this task" }));
  fireEvent.click(within(sheet.getByRole("group", { name: "Permission prompts" })).getByRole("button", { name: "Ask for approval" }));
  fireEvent.click(sheet.getByRole("button", { name: "Use this agent" })); await tick();
  expect(fix.getByText("Permission prompts: Ask for approval")).toBeTruthy();
  expect(fix.queryByRole("note")).toBeNull();
  expect(fix.getByText("--permission-mode manual").closest("details")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).toMatchObject({ github: { type: "find", fix_with: { ...opus, launch_policy: "standard" } } });
  // The fixer's flags are remembered with it for the next Find.
  expect(JSON.parse(localStorage.getItem(`github.roles.${app.profile.profileId}`)!)[ids.pTide].fix).toEqual({ ...opus, launch_policy: "standard" });
});

it("sends a Find's fixes without launch flags to a computer that can't apply them, and says so", async () => {
  const caps = app.session.state.get().capabilities!;
  app.session.state.set(s => ({ ...s, capabilities: { ...caps, features: caps.features.filter(f => f !== "fix_launch_v1") } }));
  app.ui.saveDraft(ids.pTide, null, { text: "Find issues in the project", mode: "plan", agent: opus, review: { title: "", stopWhen: "", placement: null, authorization: "ask_before_start", github: { type: "find", scope: { scope: "project" }, fix_with: { ...opus, launch_policy: "bypass_permissions" } } } });
  review(); await tick();
  const fix = within(screen.getByText("Fix with").closest("section")!);
  expect(fix.getByText(/the fixes ask for approval as before/)).toBeTruthy();
  fireEvent.click(fix.getByRole("button", { name: /Claude Code/ })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "Agent for this task" }));
  expect(sheet.queryByText("Permission prompts")).toBeNull();
  fireEvent.click(sheet.getByRole("button", { name: "Close" })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).toMatchObject({ github: { type: "find", fix_with: opus } });
  expect((created() as Extract<Command, { command: "create_task" }>).github).not.toHaveProperty("fix_with.launch_policy");
  // Nor can anything else send them there.
  const body = { ...created()!, github: { type: "find", scope: { scope: "project" }, fix_with: { ...opus, launch_policy: "bypass_permissions" } } } as Command;
  const before = app.session.commands.get();
  await expect(app.session.command(body, { label: "Start task" })).rejects.toThrow(/fixes ask for approval as before/);
  expect(app.session.commands.get()).toEqual(before);
});

it("doesn't remember this phone's launch defaults as the fixer's flags when the computer can't offer them", async () => {
  const caps = app.session.state.get().capabilities!;
  app.session.state.set(s => ({ ...s, capabilities: { ...caps, features: caps.features.filter(f => f !== "fix_launch_v1") } }));
  app.ui.saveDraft(ids.pTide, null, { text: "Find issues in the project", mode: "plan", agent: opus, review: { title: "", stopWhen: "", placement: null, authorization: "ask_before_start", github: { type: "find", scope: { scope: "project" }, fix_with: opus } } });
  review(); await tick();
  fireEvent.click(within(screen.getByText("Fix with").closest("section")!).getByRole("button", { name: /Claude Code/ })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: "Agent for this task" })).getByRole("button", { name: "Use this agent" })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect((created() as Extract<Command, { command: "create_task" }>).github).not.toHaveProperty("fix_with.launch_policy");
  // So once the computer is updated, the next Find follows this phone's defaults then.
  expect(JSON.parse(localStorage.getItem(`github.roles.${app.profile.profileId}`)!)[ids.pTide].fix).toEqual(opus);
});

// ---- A project's GitHub --------------------------------------------------------

// The repository is read first, then its issues.
const hub = async (shown: "issues" | "find_fix" = "issues") => {
  const route = { screen: "github", projectId: ids.pTide, view: shown } as const;
  const mounted = view(<Github route={route} />, route); await tick(); await tick(); return mounted;
};
const newOne = async (kind: "issue" | "review" | "review_only") => {
  const route = { screen: "github_new", projectId: ids.pTide, kind } as const;
  const mounted = view(<GithubNew route={route} />, route); await tick(); await tick(); return mounted;
};
/** The app starts again: its interface is read back from storage. */
const restart = () => { app.ui.persistNow(); app.ui.retire(); app = { ...app, ui: new UiStore(app.profile.profileId) }; };
const roles = (r: { find?: object; fix?: object; review?: object }) => localStorage.setItem(`github.roles.${app.profile.profileId}`, JSON.stringify({ [ids.pTide]: r }));

it("sits first among the project's doors in Tasks", async () => {
  const route = { screen: "work" } as const;
  app.ui.selectProject(ids.pTide);
  render(<AppProvider app={app}><RouteProvider value={{ tab: "work", route, depth: 0, active: true }}><Work route={route} /></RouteProvider></AppProvider>);
  await tick();
  const doors = within(screen.getByRole("heading", { name: "Project" }).closest("section")!).getAllByRole("button").map((b) => b.textContent);
  expect(doors[0]).toMatch(/^Issues & pull requests/);
  expect(doors[1]).toMatch(/^Find & fix/);
  expect(doors.some((d) => d?.startsWith("GitHub settings"))).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: /^Find & fix/ }));
  let ui = app.ui.get();
  expect(ui.stacks[ui.tab].at(-1)).toEqual({ screen: "github", projectId: ids.pTide, view: "find_fix" });
  // One of the agents' pull requests is open, so the one door goes to them.
  fireEvent.click(screen.getByRole("button", { name: /^Issues & pull requests/ }));
  ui = app.ui.get();
  expect(ui.stacks[ui.tab].at(-1)).toEqual({ screen: "github_list", projectId: ids.pTide });
});

it("switches between pull requests and issues in place, so Back still leaves both", async () => {
  app.ui.selectTab("work");
  app.ui.push({ screen: "github_list", projectId: ids.pTide });
  const depth = app.ui.get().stacks.work.length;
  view(<GithubList route={{ screen: "github_list", projectId: ids.pTide }} />, { screen: "github_list", projectId: ids.pTide }); await tick();
  expect(screen.getByRole<HTMLInputElement>("radio", { name: "Pull requests" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(screen.getByRole("radio", { name: "Issues" }));
  expect(app.ui.get().stacks.work).toHaveLength(depth);
  expect(app.ui.route()).toEqual({ screen: "github", projectId: ids.pTide, view: "issues" });
  cleanup();
  await hub();
  fireEvent.click(screen.getByRole("radio", { name: "Pull requests" }));
  expect(app.ui.get().stacks.work).toHaveLength(depth);
  expect(app.ui.route()).toEqual({ screen: "github_list", projectId: ids.pTide });
  app.ui.back();
  expect(app.ui.route()).toEqual({ screen: "work" });
});

it("asks what's new first in Tasks: a task, a find & fix, an issue or a pull request, each on its own screen", async () => {
  app.ui.selectProject(ids.pTide);
  app.ui.selectTab("work");
  view(<NewWork project={app.session.state.get().projects[ids.pTide]!} />, { screen: "work" }); await tick();
  expect(screen.queryByRole("button", { name: /^New task/ })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /^New/ })); await tick();
  let sheet = within(screen.getByRole("dialog", { name: "New" }));
  expect(sheet.getAllByRole("button").map((b) => b.textContent).filter(Boolean)).toEqual([
    expect.stringMatching(/^New task/), expect.stringMatching(/^New find & fix/), expect.stringMatching(/^New issue/), expect.stringMatching(/^New pull request/),
    expect.stringMatching(/^Scan a Clankerhub code/),
  ]);
  // Scanning a handoff's code is New's too; the camera takes over from the sheet.
  const scan = vi.spyOn(app.platform, "scanPairingCode").mockResolvedValue(null);
  fireEvent.click(sheet.getByRole("button", { name: "Scan a Clankerhub code" })); await tick();
  expect(scan).toHaveBeenCalledOnce();
  expect(screen.queryByRole("dialog", { name: "New" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /^New/ })); await tick();
  sheet = within(screen.getByRole("dialog", { name: "New" }));
  fireEvent.click(sheet.getByRole("button", { name: /^New find & fix/ })); await tick();
  expect(app.ui.get().stacks.work.at(-1)).toEqual({ screen: "github", projectId: ids.pTide, view: "find_fix" });
  fireEvent.click(screen.getByRole("button", { name: /^New/ })); await tick();
  sheet = within(screen.getByRole("dialog", { name: "New" }));
  fireEvent.click(sheet.getByRole("button", { name: /^New issue/ })); await tick();
  expect(app.ui.get().tab).toBe("work");
  expect(app.ui.get().stacks.work.at(-1)).toEqual({ screen: "github_new", projectId: ids.pTide, kind: "issue" });
  fireEvent.click(screen.getByRole("button", { name: /^New/ })); await tick();
  sheet = within(screen.getByRole("dialog", { name: "New" }));
  fireEvent.click(sheet.getByRole("button", { name: /^New task/ })); await tick();
  // A new task opens in Talk's chat, and Back from it comes back to Work.
  expect(app.ui.get().tab).toBe("talk");
  expect(app.ui.get().taskByProject[ids.pTide]).toBeNull();
  expect(app.ui.get().conversing).toBe(false);
  app.ui.leaveTask(true);
  expect(app.ui.get().tab).toBe("work");
});

it("offers a task or a find & fix in FINDINGS.md where the project can't use GitHub", async () => {
  const project = app.session.state.get().projects[ids.pGarden]!;
  app.ui.selectTab("work");
  view(<NewWork project={project} />, { screen: "work" }); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^New/ })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "New" }));
  expect(sheet.getAllByRole("button").map((b) => b.textContent).filter(Boolean)).toEqual([
    expect.stringMatching(/^New task/), expect.stringMatching(/^New find & fix.*FINDINGS\.md/), expect.stringMatching(/^Scan a Clankerhub code/),
  ]);
  fireEvent.click(sheet.getByRole("button", { name: /^New find & fix/ })); await tick();
  expect(app.ui.get().stacks.work.at(-1)).toEqual({ screen: "findings", projectId: ids.pGarden });
});

it("opens New pull request as Work asking for one, and New task leaves the project's default", async () => {
  app.ui.selectProject(ids.pTide);
  app.ui.selectTab("work");
  app.ui.saveDraft(ids.pTide, null, { text: "Add a retry budget header", agent: astra, mode: "ask" });
  view(<NewWork project={app.session.state.get().projects[ids.pTide]!} />, { screen: "work" }); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^New/ })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: "New" })).getByRole("button", { name: /^New pull request/ })); await tick();
  expect(app.ui.get().tab).toBe("talk");
  expect(app.ui.draft(ids.pTide, null)).toMatchObject({ text: "Add a retry budget header", mode: "work", review: { github: { type: "new_pr" } } });
  app.ui.leaveTask(true);
  fireEvent.click(screen.getByRole("button", { name: /^New/ })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: "New" })).getByRole("button", { name: /^New task/ })); await tick();
  expect(app.ui.draft(ids.pTide, null)?.review).not.toHaveProperty("github");
});

it("won't open a pull request for a project with GitHub off", async () => {
  window.__sim!.host.github.settings.delete(ids.pTide);
  app.ui.selectProject(ids.pTide);
  view(<NewWork project={app.session.state.get().projects[ids.pTide]!} />, { screen: "work" }); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^New/ })); await tick(); await tick();
  const pr = within(screen.getByRole("dialog", { name: "New" })).getByRole<HTMLButtonElement>("button", { name: /^New pull request/ });
  expect(pr.disabled).toBe(true);
  expect(pr.textContent).toContain("Turn GitHub on first, with Set up GitHub under New");
});

const newWork = (projectId: string) => view(<NewWork project={app.session.state.get().projects[projectId]!} />, { screen: "work" });

it("puts Set up GitHub right under New while GitHub is off, and turns it on there", async () => {
  window.__sim!.host.github.settings.delete(ids.pTide);
  newWork(ids.pTide); await tick();
  const rows = screen.getAllByRole("button").map((b) => b.textContent ?? "");
  expect(rows[0]).toMatch(/^New/);
  // The project's checklist comes first, right under New.
  expect(rows[1]).toMatch(/^Launch tidewater-api/);
  expect(rows[2]).toBe("Set up GitHubTurn it on for samrivera/tidewater-api so agents open pull requests");
  fireEvent.click(screen.getByRole("button", { name: /^Set up GitHub/ })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "GitHub settings" }));
  const on = sheet.getByRole<HTMLInputElement>("switch", { name: "Use GitHub for this project" });
  expect(on.checked).toBe(false);
  fireEvent.click(on); await tick();
  expect(github().at(-1)).toMatchObject({ action: "set_settings", project_id: ids.pTide, settings: { mode: "drafts" } });
  expect(screen.getByText("GitHub is on")).toBeTruthy();
  // The row goes once it's on; the sheet stays open on the settings that now matter.
  expect(screen.queryByRole("button", { name: /^Set up GitHub/ })).toBeNull();
  expect(sheet.getByRole<HTMLInputElement>("switch", { name: "Use GitHub for this project" }).checked).toBe(true);
  expect(sheet.getByLabelText("Branch prefix")).toBeTruthy();
});

it("shows no Set up GitHub while GitHub is on", async () => {
  newWork(ids.pTide); await tick();
  expect(screen.getByRole("button", { name: /^New/ })).toBeTruthy();
  expect(screen.queryByRole("button", { name: /^Set up GitHub/ })).toBeNull();
});

it("sends Set up GitHub to Accounts while nobody is signed in to GitHub", async () => {
  window.__sim!.host.github.login = "";
  app.ui.selectTab("work");
  newWork(ids.pTide); await tick();
  expect(screen.getByRole("button", { name: /^Set up GitHub/ }).textContent).toContain("Sign in to GitHub under Accounts first");
  fireEvent.click(screen.getByRole("button", { name: /^Set up GitHub/ })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "GitHub settings" }));
  expect(sheet.getByRole<HTMLInputElement>("switch", { name: "Use GitHub for this project" }).disabled).toBe(true);
  fireEvent.click(sheet.getByRole("button", { name: "Open Accounts" })); await tick();
  expect(app.ui.get().stacks.work.at(-1)).toEqual({ screen: "accounts" });
});

it("says why a project whose origin isn't on GitHub can't turn it on", async () => {
  newWork(ids.pNotes); await tick();
  expect(screen.getByRole("button", { name: /^Set up GitHub/ }).textContent).toContain("isn't on github.com");
  fireEvent.click(screen.getByRole("button", { name: /^Set up GitHub/ })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "GitHub settings" }));
  expect(sheet.getByRole<HTMLInputElement>("switch", { name: "Use GitHub for this project" }).disabled).toBe(true);
  expect(sheet.getByText(/origin is on github\.com/)).toBeTruthy();
});

it("says a project whose folder isn't a repository can't use GitHub, and adds the one in it as a project", async () => {
  app.ui.selectTab("work");
  newWork(ids.pGarden); await tick();
  const row = screen.getByRole("button", { name: /^Set up GitHub/ });
  expect(row.textContent).toContain("Its folder isn’t a Git repository, but app inside it is");
  fireEvent.click(row); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "GitHub settings" }));
  expect(sheet.getByRole<HTMLInputElement>("switch", { name: "Use GitHub for this project" }).disabled).toBe(true);
  expect(sheet.getByText(/The folder app inside it is one, on GitHub/)).toBeTruthy();
  fireEvent.click(sheet.getByRole("button", { name: /^Add app as a project/ })); await tick();
  const added = Object.values(app.session.state.get().projects).find((p) => p.host_path === "/home/sam/Projects/garden-planner/app")!;
  expect(added.vcs?.kind).toBe("git");
  expect(app.ui.get().projectId).toBe(added.project_id);
  expect(screen.queryByRole("dialog", { name: "GitHub settings" })).toBeNull();
  // Asked again, the folder is the project it now is.
  cleanup();
  newWork(ids.pGarden); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^Set up GitHub/ })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: "GitHub settings" })).getByRole("button", { name: /^Open app/ })); await tick();
  expect(app.ui.get().projectId).toBe(added.project_id);
});

it("offers no Set up GitHub for a one-off or the home folder", async () => {
  for (const id of [ids.pZine, ids.pHome]) {
    newWork(id); await tick();
    expect(screen.getByRole("button", { name: /^New/ })).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^Set up GitHub/ })).toBeNull();
    cleanup();
  }
});

it("turns GitHub off from GitHub settings in Project settings", async () => {
  view(<ProjectSheet project={app.session.state.get().projects[ids.pTide]!} onClose={() => {}} />, { screen: "work" }); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^GitHub settings/ })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "GitHub settings" }));
  const on = sheet.getByRole<HTMLInputElement>("switch", { name: "Use GitHub for this project" });
  expect(on.checked).toBe(true);
  fireEvent.click(on); await tick();
  expect(github().at(-1)).toMatchObject({ action: "set_settings", project_id: ids.pTide, settings: { mode: "off" } });
  expect(sheet.getByRole<HTMLInputElement>("switch", { name: "Use GitHub for this project" }).checked).toBe(false);
});

it("opens a project's GitHub settings straight from Settings › GitHub", async () => {
  const route = { screen: "github" } as const;
  view(<Github route={route} />, route); await tick();
  const name = app.session.state.get().projects[ids.pTide]!.name, push = vi.spyOn(app.ui, "push");
  fireEvent.click(screen.getByRole("button", { name: `GitHub settings for ${name}` })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "GitHub settings" }));
  expect(sheet.getByRole<HTMLInputElement>("switch", { name: "Use GitHub for this project" }).checked).toBe(true);
  // Over the list: closing the sheet leaves Settings › GitHub, not the project's issues.
  expect(push).not.toHaveBeenCalled();
});

it("opens on Issues, sorted by triage, and starts one with the agent that last fixed here", async () => {
  roles({ fix: astra });
  await hub();
  expect(screen.getByRole("heading", { name: "Issues", level: 1 })).toBeTruthy();
  expect(screen.queryByText("Sort open issues")).toBeNull();
  const section = (title: string) => within(screen.getByRole("heading", { name: title }).closest("section")!);
  expect(section("Needs you").getByText("#221 Orders export times out for large accounts")).toBeTruthy();
  fireEvent.click(section("Agent can take it").getByText("#219 Retry flake in the orders webhook test")); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "#219 Retry flake in the orders webhook test" }));
  expect(sheet.getByRole("button", { name: /^Worked by/ }).textContent).toContain("GPT-6-Astra");
  fireEvent.click(sheet.getByRole("button", { name: "Start" })); await tick();
  expect(created()).toMatchObject({
    project_id: ids.pTide, mode: "work", placement: { mode: "start_now" }, harness: "codex", model: "gpt-6-astra",
    github: { type: "issue", number: 219 },
  });
  expect(screen.getByText("Started on #219")).toBeTruthy();
  // Nothing went through Review.
  expect(app.ui.get().stacks.talk.some((r) => r.screen === "review")).toBe(false);
  // The toast opens the task it started.
  fireEvent.click(within(screen.getByText("Started on #219").closest<HTMLElement>(".toast")!).getByRole("button", { name: "Open task" })); await tick();
  const made = Object.values(app.session.state.get().tasks).find((t) => t.github?.issue === 219)!;
  expect(app.ui.get()).toMatchObject({ tab: "talk", taskByProject: { [ids.pTide]: made.task_id } });
});

it("asks an issue's agent only to work it, with any direction added, and another agent to review it", async () => {
  roles({ fix: astra });
  // Earlier words for a new task don't follow into an issue.
  app.ui.saveDraft(ids.pTide, null, { text: "Something else entirely", agent: astra, mode: "work" });
  await hub();
  fireEvent.click(screen.getByText("#219 Retry flake in the orders webhook test")); await tick();
  const sheet = within(screen.getByRole("dialog", { name: /^#219/ }));
  expect(sheet.queryByText(/Something else entirely/)).toBeNull();
  fireEvent.change(sheet.getByLabelText("More direction"), { target: { value: "  Keep the fix inside the test.  " } });
  const review = sheet.getByRole<HTMLInputElement>("checkbox", { name: "Have another agent review the code" });
  expect(review.checked).toBe(false);
  fireEvent.click(review); await tick();
  // A different agent from the one fixing it, unless chosen otherwise.
  expect(sheet.getByRole("button", { name: /^Reviewed by/ }).textContent).toContain("Claude Code");
  fireEvent.click(sheet.getByRole("button", { name: "Start" })); await tick();
  expect(created()).toMatchObject({
    harness: "codex", prompt: "Fix #219: Retry flake in the orders webhook test\n\nKeep the fix inside the test.",
    github: { type: "issue", number: 219, review_with: { harness: "claude_code" } },
  });
  const made = Object.values(app.session.state.get().tasks).find((t) => t.github?.issue === 219)!;
  expect(made.github?.check_with).toMatchObject({ harness: "claude_code" });
  expect(JSON.parse(localStorage.getItem(`github.roles.${app.profile.profileId}`)!)[ids.pTide].review).toMatchObject({ harness: "claude_code" });
});

it("leaves an issue's review to the project setting while the computer can't take a reviewer", async () => {
  roles({ fix: astra });
  const caps = app.session.state.get().capabilities!;
  app.session.state.set(s => ({ ...s, capabilities: { ...caps, features: caps.features.filter(f => f !== "github_issues_v1") } }));
  await hub();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: /^New issue/ }).disabled).toBe(true);
  fireEvent.click(screen.getByText("#219 Retry flake in the orders webhook test")); await tick();
  const sheet = within(screen.getByRole("dialog", { name: /^#219/ }));
  expect(sheet.queryByRole("checkbox", { name: "Have another agent review the code" })).toBeNull();
  fireEvent.click(sheet.getByRole("button", { name: "Start" })); await tick();
  expect((created() as Extract<Command, { command: "create_task" }>).github).not.toHaveProperty("review_with");
});

it("starts an issue when the project is free, or adds it to Next up to start later", async () => {
  roles({ fix: astra });
  const host = window.__sim!.host;
  const project = { ...app.session.state.get().projects[ids.pTide]!, active_writer: writer };
  host.projects = host.projects.map(p => p.project_id === project.project_id ? project : p);
  host.emit({ type: "project_upserted", project }); await tick();
  await hub();
  fireEvent.click(screen.getByText("#219 Retry flake in the orders webhook test")); await tick();
  let sheet = within(screen.getByRole("dialog", { name: /^#219/ }));
  fireEvent.click(sheet.getByRole("button", { name: "Start when free" })); await tick();
  expect(created()).toMatchObject({ placement: { mode: "queue", position: null, authorization: "auto_start" }, github: { type: "issue", number: 219 } });
  fireEvent.click(screen.getByText("#221 Orders export times out for large accounts")); await tick();
  sheet = within(screen.getByRole("dialog", { name: /^#221/ }));
  fireEvent.change(sheet.getByLabelText("Your answer"), { target: { value: "Stream them." } });
  fireEvent.click(sheet.getByRole("button", { name: "Add to Next up" })); await tick();
  expect(sent().filter(c => c.command === "create_task").at(-1)).toMatchObject({
    placement: { mode: "queue", position: null, authorization: "auto_start" }, github: { type: "issue", number: 221 },
  });
});

/**
 * A fixer that finished on issue `number` without a fix, as
 * `github_cannot_reproduce` leaves it: what it found, and its alert open.
 */
function checked(number: number, finding: string, alreadyFixed: boolean) {
  const host = window.__sim!.host;
  const now = Date.now();
  const repo = `${host.github.repo.owner}/${host.github.repo.name}`.toLowerCase();
  const like = host.tasks.find((t) => t.project_id === ids.pTide)!;
  const task = {
    ...like, task_id: `0192f7a1-0000-7000-8000-00000000c${number}`, title: `Fix #${number}`, state: "completed" as const, current_run: null,
    status_line: null, created_at: now, updated_at: now, revision: 1, attention_count: 1,
    github: { link: { type: "issue" as const, number }, issue: number, stage: "not_reproduced" as const, rounds: 0, repo, finding, already_fixed: alreadyFixed },
  };
  const alert = {
    attention_id: `0192f7a1-0000-7000-8000-00000000d${number}`, kind: "waiting_for_you" as const,
    title: alreadyFixed ? `#${number} looks fixed already: close it?` : `Couldn’t reproduce #${number}`, body: finding,
    task_id: task.task_id, run_id: null, approval_id: null, created_at: now, resolved_at: null, priority: 3,
  };
  host.tasks = [...host.tasks, task];
  host.attention = [...host.attention, alert];
  host.emit({ type: "task_upserted", task });
  host.emit({ type: "attention_upserted", item: alert });
  return task;
}

it("says what an issue an agent found already fixed needs, and closes it in one tap", async () => {
  const task = checked(219, "Already fixed by PR #27, merged as `ace69efe`.", true);
  await tick();
  await hub();
  const section = within(screen.getByRole("heading", { name: "Needs you" }).closest("section")!);
  const row = section.getByText("#219 Retry flake in the orders webhook test").closest<HTMLElement>("button")!;
  // The row says what happened and the step left, not the agent's own labels.
  expect(within(row).getByText("The agent found this already fixed")).toBeTruthy();
  expect(within(row).getByText("Close it?")).toBeTruthy();
  fireEvent.click(row); await tick();
  const sheet = within(screen.getByRole("dialog", { name: /^#219/ }));
  expect(sheet.getByText("The agent found this already fixed")).toBeTruthy();
  expect(sheet.getByText("What the agent found")).toBeTruthy();
  expect(sheet.getByText(/Already fixed by PR #27/)).toBeTruthy();
  expect(sheet.getByText("What to do")).toBeTruthy();
  // Nothing to start until the person says it still happens.
  expect(sheet.queryByRole("button", { name: "Start" })).toBeNull();
  expect(sheet.queryByLabelText("More direction")).toBeNull();
  fireEvent.click(sheet.getByRole("button", { name: "Close issue" })); await tick();
  expect(github().at(-1)).toEqual({ action: "close_issue", project_id: ids.pTide, repo: "samrivera/tidewater-api", number: 219, done: true });
  expect(screen.getByText("Closed #219")).toBeTruthy();
  expect(screen.queryByRole("dialog", { name: /^#219/ })).toBeNull();
  const after = app.session.state.get();
  expect(after.tasks[task.task_id]!.github?.stage).toBe("closed");
  expect(after.tasks[task.task_id]!.attention_count).toBe(0);
  await tick();
  expect(screen.queryByText("#219 Retry flake in the orders webhook test")).toBeNull();
});

it("takes a closed issue off its list at once, and puts it back with why when GitHub refuses", async () => {
  checked(219, "Already fixed by PR #27.", true);
  await tick();
  await hub();
  const host = window.__sim!.host;
  const real = host.github.act.bind(host.github);
  // GitHub takes three seconds, then refuses.
  vi.spyOn(host.github, "act").mockImplementation((a) => a.action !== "close_issue" ? real(a)
    : new Promise((_, refuse) => setTimeout(() => refuse({ code: "conflict", message: "GitHub didn't answer." }), 3000)));
  fireEvent.click(screen.getByText("#219 Retry flake in the orders webhook test")); await act(async () => {});
  fireEvent.click(within(screen.getByRole("dialog", { name: /^#219/ })).getByRole("button", { name: "Close issue" }));
  // Before the computer has answered.
  expect(screen.queryByText("#219 Retry flake in the orders webhook test")).toBeNull();
  expect([...document.querySelectorAll(".toast__text")].at(-1)?.textContent).toBe("Closed #219");
  await tick(); await tick(); await tick(); await tick();
  expect(screen.getByText("#219 Retry flake in the orders webhook test")).toBeTruthy();
  const refused = screen.getByText(/^#219 is still open\. .*GitHub didn't answer/);
  expect(refused.closest(".toast")!.className).toContain("toast--stop");
});

it("tries an issue the agent couldn't reproduce again, told how to make it happen", async () => {
  roles({ fix: astra });
  checked(219, "Ran the webhook test 200 times; it passed every time.", false);
  await tick();
  await hub();
  fireEvent.click(screen.getByText("#219 Retry flake in the orders webhook test")); await tick();
  const sheet = within(screen.getByRole("dialog", { name: /^#219/ }));
  expect(sheet.getByText("The agent couldn’t make this happen")).toBeTruthy();
  expect(sheet.getByText(/passed every time/)).toBeTruthy();
  fireEvent.click(sheet.getByRole("button", { name: "It still happens" })); await tick();
  expect(sheet.getByText(/Say how to make it happen below/)).toBeTruthy();
  fireEvent.change(sheet.getByLabelText("How to make it happen"), { target: { value: "Run it with the orders queue full." } });
  fireEvent.click(sheet.getByRole("button", { name: "Start" })); await tick();
  expect(created()).toMatchObject({
    prompt: "Fix #219: Retry flake in the orders webhook test\n\nRun it with the orders queue full.", github: { type: "issue", number: 219 },
  });
});

it("closes an issue it couldn't reproduce as not planned, and on GitHub where the computer can't", async () => {
  checked(219, "Ran it 200 times.", false);
  await tick();
  await hub();
  fireEvent.click(screen.getByText("#219 Retry flake in the orders webhook test")); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: /^#219/ })).getByRole("button", { name: "Close issue" })); await tick();
  expect(github().at(-1)).toMatchObject({ action: "close_issue", number: 219, done: false });
  cleanup();
  checked(221, "Ran it 200 times.", false);
  const caps = app.session.state.get().capabilities!;
  app.session.state.set(s => ({ ...s, capabilities: { ...caps, features: caps.features.filter(f => f !== "github_close_v1") } }));
  await hub();
  fireEvent.click(screen.getByText("#221 Orders export times out for large accounts")); await tick();
  const sheet = within(screen.getByRole("dialog", { name: /^#221/ }));
  expect(sheet.queryByRole("button", { name: "Close issue" })).toBeNull();
  expect(sheet.getByText(/Update Øphio on this computer to close issues/)).toBeTruthy();
  const open = vi.spyOn(app.platform, "openExternal").mockResolvedValue(undefined);
  fireEvent.click(sheet.getByRole("button", { name: "Close on GitHub" })); await tick();
  expect(open).toHaveBeenCalledWith("https://github.com/samrivera/tidewater-api/issues/221");
});

it("says an issue an agent checked before waits for the person, and why one the Sort put there does", async () => {
  window.__sim!.host.github.items.find((i) => i.number === 219)!.labels.push("agent", "agent-not-reproduced");
  Object.assign(window.__sim!.host.github.items.find((i) => i.number === 219)!, { group: "needs_you", triage_note: "An agent checked this before." });
  await hub();
  const row = screen.getByText("#219 Retry flake in the orders webhook test").closest<HTMLElement>("button")!;
  expect(within(row).getByText("An agent already checked this")).toBeTruthy();
  expect(row.textContent).toContain("@samrivera · bug ·");
  expect(row.textContent).not.toContain("agent-");
  fireEvent.click(row); await tick();
  let sheet = within(screen.getByRole("dialog", { name: /^#219/ }));
  expect(sheet.getByRole("button", { name: "Read its comment on GitHub" })).toBeTruthy();
  expect(sheet.getByRole("button", { name: "Close issue" })).toBeTruthy();
  fireEvent.click(sheet.getByRole("button", { name: "Close issue" })); await tick();
  expect(github().at(-1)).toMatchObject({ action: "close_issue", number: 219, done: false });
  fireEvent.click(screen.getByText("#221 Orders export times out for large accounts")); await tick();
  sheet = within(screen.getByRole("dialog", { name: /^#221/ }));
  expect(sheet.getByText("An agent can’t start this until you answer")).toBeTruthy();
  expect(sheet.getByText("The Sort asks")).toBeTruthy();
  expect(sheet.getByText(/^Should big exports page through the orders/)).toBeTruthy();
  // Deciding it still starts here, or closes it.
  expect(sheet.getByRole("button", { name: "Close issue" })).toBeTruthy();
  // Not until the person answers, and the sheet says so.
  const start = sheet.getByRole<HTMLButtonElement>("button", { name: "Start" });
  expect(start.disabled).toBe(true);
  expect(sheet.getByRole<HTMLButtonElement>("button", { name: "Add to Next up" }).disabled).toBe(true);
  expect(sheet.getByText("Start and Add to Next up wait for your answer above.")).toBeTruthy();
  expect(sheet.queryByLabelText("More direction")).toBeNull();
  fireEvent.change(sheet.getByLabelText("Your answer"), { target: { value: "   " } });
  expect(start.disabled).toBe(true);
  // The Sort's own answer is a start for the person's, to read and edit; it doesn't start anything.
  expect(sheet.getByText("The Sort would answer: Stream them")).toBeTruthy();
  fireEvent.change(sheet.getByLabelText("Your answer"), { target: { value: "" } });
  fireEvent.click(sheet.getByRole("button", { name: "Use as my answer" })); await tick();
  expect((sheet.getByLabelText("Your answer") as HTMLTextAreaElement).value).toBe("Stream them");
  expect(created()).toBeUndefined();
  expect(sheet.queryByRole("button", { name: "Use as my answer" })).toBeNull();
  fireEvent.change(sheet.getByLabelText("Your answer"), { target: { value: "Stream them, and say so in the changelog." } });
  expect(start.disabled).toBe(false);
  fireEvent.click(start); await tick();
  // The agent is told what the Sort asked, since it doesn't see the Sort's note.
  const prompt = (created() as Extract<Command, { command: "create_task" }>).prompt;
  expect(prompt).toMatch(/^Fix #221: Orders export times out for large accounts\n\nThe Sort asked: “Should big exports page through the orders/);
  expect(prompt).toMatch(/”\n\nMy answer: Stream them, and say so in the changelog\.$/);
});

it("starts a stranger's issue in Plan", async () => {
  roles({ fix: opus });
  await hub();
  fireEvent.click(screen.getByText("#223 Rate limits page is out of date")); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "#223 Rate limits page is out of date" }));
  expect(sheet.getByText("From someone who can’t write to the repository")).toBeTruthy();
  fireEvent.click(sheet.getByRole("button", { name: "Start" })); await tick();
  expect(created()).toMatchObject({ mode: "plan", prompt: "Fix #223: Rate limits page is out of date", github: { type: "issue", number: 223 } });
});

it("merges once checks pass only after the warning is confirmed, and turns it off in one tap", async () => {
  await hub();
  const merge = () => screen.getByRole<HTMLInputElement>("switch", { name: "Merge once checks pass" });
  expect(merge().checked).toBe(false);
  fireEvent.click(merge()); await tick();
  let ask = within(screen.getByRole("dialog", { name: "Merge once checks pass?" }));
  const risk = ask.getByText((_, el) => el?.tagName === "LI" && /^Nothing protects main,/.test(el.textContent ?? ""));
  expect(risk.querySelector("code")?.textContent).toBe("main");
  const before = github().length;
  fireEvent.click(ask.getByRole("button", { name: "Not now" })); await tick();
  expect(github()).toHaveLength(before);
  expect(merge().checked).toBe(false);

  fireEvent.click(merge()); await tick();
  ask = within(screen.getByRole("dialog", { name: "Merge once checks pass?" }));
  fireEvent.click(ask.getByRole("button", { name: "Merge once checks pass" })); await tick();
  expect(github().slice(-2)).toEqual([
    expect.objectContaining({ action: "set_settings", project_id: ids.pTide, settings: expect.objectContaining({ mode: "through_merge" }) }),
    { action: "answer_warning", project_id: ids.pTide, repo: "samrivera/tidewater-api", keep_through_merge: true },
  ]);
  expect(merge().checked).toBe(true);
  expect(screen.queryByText("Before it merges by itself")).toBeNull();
  expect(screen.getByText(/The agent merges its pull requests into main once their checks pass, without asking/)).toBeTruthy();

  fireEvent.click(merge()); await tick();
  expect(github().at(-1)).toMatchObject({ action: "set_settings", settings: { mode: "drafts" } });
  expect(merge().checked).toBe(false);
});

it("asks for the warning still unanswered when merging was turned on at the computer", async () => {
  const host = window.__sim!.host;
  host.github.settings.set(ids.pTide, { ...host.github.settings.get(ids.pTide)!, mode: "through_merge", warning_answered: false });
  await hub();
  expect(screen.getByText("Before it merges by itself")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "I’ll merge them" })); await tick();
  expect(github().at(-1)).toEqual({ action: "answer_warning", project_id: ids.pTide, repo: "samrivera/tidewater-api", keep_through_merge: false });
  expect(screen.getByRole<HTMLInputElement>("switch", { name: "Merge once checks pass" }).checked).toBe(false);
});

it("turns GitHub on from the one notice shown while it's off", async () => {
  window.__sim!.host.github.settings.delete(ids.pTide);
  await hub();
  expect(screen.queryByRole("switch", { name: "Merge once checks pass" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Turn on" })); await tick();
  expect(github().at(-1)).toMatchObject({ action: "set_settings", project_id: ids.pTide, settings: { mode: "drafts" } });
  expect(screen.queryByText("GitHub is off for this project")).toBeNull();
  expect(screen.getByRole("switch", { name: "Merge once checks pass" })).toBeTruthy();
});

it("reviews as the chosen profile, files up to the limit, and queues the fixes for the chosen fixer", async () => {
  roles({ find: opus, fix: astra });
  app.ui.push({ screen: "github", projectId: ids.pTide, view: "find_fix" }, "work");
  app.ui.push({ screen: "github_new", projectId: ids.pTide, kind: "review" });
  await newOne("review");
  expect(screen.getByRole("heading", { name: "Review and fix", level: 1 })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Security researcher" }));
  expect(screen.getByRole("button", { name: /^Reviews/ }).textContent).toContain("Claude Code");
  expect(screen.getByRole("button", { name: /^Fixes/ }).textContent).toContain("GPT-6-Astra");
  expect(screen.getByText("5", { selector: "output" })).toBeTruthy();
  const plus = screen.getByRole("button", { name: "One more" });
  fireEvent.click(plus); fireEvent.click(plus);
  expect(screen.getByText("7", { selector: "output" })).toBeTruthy();
  fireEvent.click(within(screen.getByRole("radiogroup", { name: "After filing" })).getByRole("radio", { name: "Queue fixes" }));
  expect(screen.getByText(/It files up to 7, most serious first, then stops/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start review" })); await tick();
  expect(created()).toMatchObject({
    mode: "plan", placement: { mode: "start_now" }, harness: "claude_code", prompt: "Find security issues in the project",
    github: { type: "find", scope: { scope: "project" }, focus: "security", max_issues: 7, fix_findings: "ask_first", fix_with: { harness: "codex", model: "gpt-6-astra" } },
  });
  expect(screen.getByText("Review started")).toBeTruthy();
  expect(JSON.parse(localStorage.getItem(`github.roles.${app.profile.profileId}`)!)[ids.pTide].find).toMatchObject(opus);
  // Back where New review was opened from.
  expect(app.ui.get().stacks.work.at(-1)).toEqual({ screen: "github", projectId: ids.pTide, view: "find_fix" });
});

it("remembers the most issues to file for each project on this phone", async () => {
  roles({ find: opus, fix: astra });
  const first = await newOne("review");
  fireEvent.click(screen.getByRole("button", { name: "One more" }));
  expect(JSON.parse(localStorage.getItem(`github.limits.${app.profile.profileId}`)!)).toEqual({ [ids.pTide]: 6 });
  first.unmount();
  await newOne("review");
  expect(screen.getByText("6", { selector: "output" })).toBeTruthy();
  expect(screen.getByText(/It files up to 6, most serious first/)).toBeTruthy();
});

it("files only, with no fixer, from Review only", async () => {
  roles({ find: opus, fix: astra });
  await newOne("review_only");
  expect(screen.getByRole("heading", { name: "Review only", level: 1 })).toBeTruthy();
  expect(screen.queryByRole("button", { name: /^Fixes/ })).toBeNull();
  expect(screen.queryByRole("radiogroup", { name: "After filing" })).toBeNull();
  fireEvent.click(screen.getByRole("radio", { name: "A folder" }));
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Start review" }).disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("Folder"), { target: { value: "src/orders" } });
  fireEvent.click(screen.getByRole("button", { name: "Start review" })); await tick();
  expect(created()).toMatchObject({ prompt: "Find issues in src/orders", github: { type: "find", scope: { scope: "folder", path: "src/orders" }, fix_findings: "file_only" } });
  expect((created() as Extract<Command, { command: "create_task" }>).github).not.toHaveProperty("focus");
});

it("offers a computer without reviews its four profiles and no limit, and says why", async () => {
  roles({ find: opus, fix: astra });
  const caps = app.session.state.get().capabilities!;
  app.session.state.set(s => ({ ...s, capabilities: { ...caps, features: caps.features.filter(f => f !== "github_review_v1") } }));
  await newOne("review");
  expect(screen.getByText(/choose a review’s profile, how many issues it files and what happens next/)).toBeTruthy();
  expect(within(screen.getByRole("group", { name: "Reviewers who read the code" })).getAllByRole("button")).toHaveLength(5);
  expect(screen.queryByRole("button", { name: "One more" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Start review" })); await tick();
  const link = (created() as Extract<Command, { command: "create_task" }>).github!;
  expect(link).not.toHaveProperty("max_issues");
  expect(link).not.toHaveProperty("fix_findings");
});

it("reviews as a novice user on the one free screen, through an agent that can use it", async () => {
  roles({ find: astra, fix: astra });
  await newOne("review");
  fireEvent.click(within(screen.getByRole("group", { name: "Uses the app" })).getByRole("button", { name: "Novice user" })); await tick();
  expect(screen.queryByRole("radio", { name: "A folder" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Edit first" })).toBeNull();
  // DP-1 is held by another run and the window can't take input, so the one free display is chosen.
  const screens = within(screen.getByRole("radiogroup", { name: "Screen it uses" }));
  expect(screens.getByRole("radio", { name: /HDMI-A-1/ }).getAttribute("aria-checked")).toBe("true");
  expect(screens.getByRole("radio", { name: /^Screen 1 \(DP-1\)/ }).textContent).toContain("Input is already held");
  expect(screens.getByRole("radio", { name: /Firefox/ }).textContent).toContain("control DP-1 instead");
  // The agent new work here goes to, the project's Claude Code, uses the screen until another is chosen.
  expect(screen.getByRole("button", { name: /^Uses it/ }).textContent).toContain("Claude Code");
  fireEvent.click(screen.getByRole("button", { name: /^Uses it/ })); await tick();
  const agents = within(screen.getByRole("group", { name: "Agent" }));
  expect(agents.getAllByRole("button").map(b => b.textContent)).toEqual([expect.stringContaining("Claude Code"), expect.stringContaining("Codex")]);
  fireEvent.click(agents.getByRole("button", { name: /Codex/ })); await tick();
  expect(screen.queryByRole("group", { name: "Effort" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Use this agent" })); await tick();
  expect(screen.getByRole("button", { name: /^Uses it/ }).textContent).toContain("Codex");
  fireEvent.change(screen.getByLabelText("How to open it"), { target: { value: "http://localhost:5173" } });
  fireEvent.click(screen.getByRole("button", { name: "Start review" })); await tick();
  expect(created()).toMatchObject({
    kind: "computer_use", mode: "work", harness: "codex", effort: null, target_id: ids.tgHdmi, placement: { mode: "start_now" },
    prompt: "Try the app as a novice user\n\nHow to open it: http://localhost:5173",
    github: { type: "find", focus: "novice_user", scope: { scope: "project" }, fix_with: { harness: "codex", model: "gpt-6-astra" } },
  });
  expect(created()).not.toHaveProperty("launch_policy");
  expect(JSON.parse(localStorage.getItem(`github.roles.${app.profile.profileId}`)!)[ids.pTide].use).toMatchObject({ harness: "codex" });
});

it("tries only what's new when asked, and won't start while the screen is out of reach", async () => {
  roles({ find: opus, fix: astra });
  await newOne("review");
  fireEvent.click(screen.getByRole("radio", { name: "A folder" }));
  fireEvent.click(screen.getByRole("button", { name: "Power user" })); await tick();
  fireEvent.click(within(screen.getByRole("radiogroup", { name: "What it uses" })).getByRole("radio", { name: "What’s new" }));
  expect(screen.getByRole("radio", { name: /HDMI-A-1/ }).getAttribute("aria-checked")).toBe("true");
  const caps = app.session.state.get().capabilities!;
  app.session.state.set(s => ({ ...s, capabilities: { ...caps, computer_use: { state: "temporarily_unavailable", reason: "Desktop sharing is off" } } })); await tick();
  expect(screen.getByText(/Computer use is unavailable on .*Desktop sharing is off/)).toBeTruthy();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Start review" }).disabled).toBe(true);
  app.session.state.set(s => ({ ...s, capabilities: caps })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Start review" })); await tick();
  expect(created()).toMatchObject({ kind: "computer_use", prompt: "Try what’s new in the app as a power user", github: { focus: "power_user", scope: { scope: "since_last_run" } } });
});

it("unlocks a locked screen with its password right where it's chosen, then starts on it", async () => {
  roles({ find: astra, fix: astra });
  await newOne("review");
  fireEvent.click(within(screen.getByRole("group", { name: "Uses the app" })).getByRole("button", { name: "Novice user" })); await tick();
  act(() => window.__sim!.control.lock(true)); await tick();
  const hdmi = () => screen.getByRole("radio", { name: /HDMI-A-1/ });
  expect(hdmi().textContent).toContain("Unlock it here with its password, or at the computer");
  expect(hdmi().getAttribute("aria-checked")).not.toBe("true");
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Start review" }).disabled).toBe(true);
  // The unlock sits in the Screen section, under the screens it opens.
  const section = hdmi().closest("section")!;
  fireEvent.click(within(section).getByRole("button", { name: "Unlock with password" }));
  const field = within(section).getByLabelText(/password$/) as HTMLInputElement;
  expect(field.type).toBe("password");
  fireEvent.change(field, { target: { value: SIM_DESKTOP_PASSWORD } });
  fireEvent.submit(field.closest("form")!);
  await tick(); await tick();
  expect(Object.values(app.session.commands.get()).filter(c => c.body.command === "unlock_desktop")).toHaveLength(1);
  expect(hdmi().getAttribute("aria-checked")).toBe("true");
  expect(within(section).queryByRole("button", { name: "Unlock with password" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Start review" })); await tick();
  expect(created()).toMatchObject({ kind: "computer_use", target_id: ids.tgHdmi });
});

it("offers no reviewers that use the app to a computer that can't run them", async () => {
  const caps = app.session.state.get().capabilities!;
  app.session.state.set(s => ({ ...s, capabilities: { ...caps, features: caps.features.filter(f => f !== "github_uat_v1") } }));
  await newOne("review");
  expect(screen.queryByText("Uses the app")).toBeNull();
  expect(screen.queryByRole("button", { name: "Novice user" })).toBeNull();
});

it("starts reviews and sorts issues from Find & fix, and lists the reviews so far", async () => {
  roles({ find: opus });
  await hub("find_fix");
  expect(screen.getByRole("heading", { name: "Find & fix", level: 1 })).toBeTruthy();
  expect(screen.queryByRole("switch", { name: "Merge once checks pass" })).toBeNull();
  expect(screen.getByText("No reviews yet")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /^Review and fix/ }));
  expect(app.ui.get().stacks.talk.at(-1)).toEqual({ screen: "github_new", projectId: ids.pTide, kind: "review" });
  fireEvent.click(screen.getByRole("button", { name: /^Review only/ }));
  expect(app.ui.get().stacks.talk.at(-1)).toEqual({ screen: "github_new", projectId: ids.pTide, kind: "review_only" });
  fireEvent.click(screen.getByRole("button", { name: /^Fix only/ }));
  expect(app.ui.get().stacks.talk.at(-1)).toEqual({ screen: "github", projectId: ids.pTide, view: "issues" });
  fireEvent.click(screen.getByRole("button", { name: /^Without GitHub/ }));
  expect(app.ui.get().stacks.talk.at(-1)).toEqual({ screen: "findings", projectId: ids.pTide });
  fireEvent.click(screen.getByRole("button", { name: /^Sort open issues/ })); await tick();
  expect(app.ui.draft(ids.pTide, null)).toMatchObject({ mode: "plan", review: { github: { type: "triage" } } });
  expect(app.ui.get().stacks.talk.at(-1)).toMatchObject({ screen: "review", intent: "new_task" });
});

// ---- New issue -------------------------------------------------------------------

const fileIssue = (title: string, body = "") => {
  fireEvent.change(screen.getByLabelText("Title"), { target: { value: title } });
  if (body) fireEvent.change(screen.getByLabelText("Description"), { target: { value: body } });
};

it("files a new issue in the person's name, then starts an agent on it", async () => {
  roles({ fix: astra });
  app.ui.push({ screen: "github_new", projectId: ids.pTide, kind: "issue" });
  await newOne("issue");
  expect(screen.getByRole("heading", { name: "New issue", level: 1 })).toBeTruthy();
  const go = () => screen.getByRole<HTMLButtonElement>("button", { name: "File and start" });
  expect(go().disabled).toBe(true);
  fileIssue("x".repeat(257));
  expect(screen.getByText("256 characters at most")).toBeTruthy();
  expect(go().disabled).toBe(true);
  fileIssue("Orders export drops the last page", "Seen on accounts with over 10k orders.");
  fireEvent.click(go()); await tick();
  expect(github().at(-1)).toEqual({
    action: "file_issue", project_id: ids.pTide, repo: "samrivera/tidewater-api", title: "Orders export drops the last page", body: "Seen on accounts with over 10k orders.",
  });
  expect(created()).toMatchObject({
    mode: "work", placement: { mode: "start_now" }, harness: "codex", prompt: "Fix #224: Orders export drops the last page",
    github: { type: "issue", number: 224 },
  });
  expect(taskTitled("Orders export drops the last page").github?.issue).toBe(224);
  expect(screen.getByText("Filed #224 and started on it")).toBeTruthy();
  expect(app.ui.get().stacks.talk.some((r) => r.screen === "github_new")).toBe(false);
  expect(window.__sim!.host.github.items[0]).toMatchObject({ number: 224, author: "samrivera", pr: false });
});

it("files an issue only, or adds it to Next up to start by itself", async () => {
  roles({ fix: astra });
  await newOne("issue");
  fileIssue("Rate limit docs need the new headers");
  fireEvent.click(within(screen.getByRole("radiogroup", { name: "Then" })).getByRole("radio", { name: "Just file it" })); await tick();
  expect(screen.queryByRole("button", { name: /^Worked by/ })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "File issue" })); await tick();
  expect(github().at(-1)).toMatchObject({ action: "file_issue", title: "Rate limit docs need the new headers" });
  expect(created()).toBeUndefined();
  expect(screen.getByText("Filed #224")).toBeTruthy();
  cleanup();
  await newOne("issue");
  fileIssue("Webhook retries ignore the budget");
  fireEvent.click(within(screen.getByRole("radiogroup", { name: "Then" })).getByRole("radio", { name: "Add to Next up" })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "File and add to Next up" })); await tick();
  expect(created()).toMatchObject({ placement: { mode: "queue", position: null, authorization: "auto_start" }, github: { type: "issue", number: 225 } });
});

it("suggests a reviewer that can read the code without changing it", async () => {
  // This computer's Codex can't promise a read-only run, so Claude Code
  // reviews its own work rather than Codex being offered and refused.
  roles({ fix: opus });
  await newOne("issue");
  fireEvent.click(screen.getByRole("checkbox", { name: "Have another agent review the code" })); await tick();
  expect(screen.getByRole("button", { name: /^Reviewed by/ }).textContent).toContain("Claude Code");
  expect(screen.queryByText(/cannot confirm that Ask and Plan/)).toBeNull();
});

it("never files an issue twice: if the work doesn't start, trying again only starts it", async () => {
  roles({ fix: astra });
  const command = app.session.command.bind(app.session);
  let refuse = true;
  vi.spyOn(app.session, "command").mockImplementation(async (body, opts) => {
    if (body.command === "create_task" && refuse) { refuse = false; throw new Error("The computer stopped answering"); }
    return command(body, opts);
  });
  const mounted = await newOne("issue");
  fileIssue("Orders export drops the last page");
  fireEvent.click(screen.getByRole("checkbox", { name: "Have another agent review the code" })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "File and start" })); await tick();
  expect(screen.getByText("#224 is filed on GitHub")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /^Discard/ })).toBeNull();
  // Leaving and coming back, even after a restart, still knows it's filed and who reviews it.
  mounted.unmount();
  restart();
  await newOne("issue");
  expect(screen.getByText("#224 is filed on GitHub")).toBeTruthy();
  expect(screen.getByLabelText<HTMLInputElement>("Title").disabled).toBe(true);
  expect(screen.getByRole<HTMLInputElement>("checkbox", { name: "Have another agent review the code" }).checked).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Start work on #224" })); await tick();
  expect(github().filter((a) => a.action === "file_issue")).toHaveLength(1);
  expect(created()).toMatchObject({ github: { type: "issue", number: 224, review_with: { harness: "claude_code" } } });
  expect(app.ui.get().issueDrafts[ids.pTide]).toBeUndefined();
});

it("files an issue once while its answer is on the way, even from a New issue opened again meanwhile", async () => {
  roles({ fix: astra });
  const command = app.session.command.bind(app.session);
  let answer!: () => void;
  const late = new Promise<void>((r) => { answer = r; });
  vi.spyOn(app.session, "command").mockImplementation(async (body, opts) => {
    if (body.command === "github" && body.action.action === "file_issue") await late;
    return command(body, opts);
  });
  const mounted = await newOne("issue");
  fileIssue("Orders export drops the last page");
  fireEvent.click(screen.getByRole("button", { name: "File and start" })); await tick();
  mounted.unmount();
  await newOne("issue");
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "File and start" }).disabled).toBe(true);
  expect(screen.getByLabelText<HTMLInputElement>("Title").disabled).toBe(true);
  expect(screen.getByRole<HTMLButtonElement>("button", { name: /^Discard/ }).disabled).toBe(true);
  // It can't be discarded and replaced meanwhile; one that was is left alone.
  app.ui.clearIssueDraft(ids.pTide);
  app.ui.saveIssueDraft(ids.pTide, { title: "Webhook retries ignore the budget" });
  answer(); await tick(); await tick();
  expect(github().filter((a) => a.action === "file_issue")).toHaveLength(1);
  expect(created()).toMatchObject({ github: { type: "issue", number: 224 } });
  expect(app.ui.get().issueDrafts[ids.pTide]).toMatchObject({ title: "Webhook retries ignore the budget", filed: null, pending: null });
});

it("says an issue may be filed already when the app closed before GitHub answered", async () => {
  roles({ fix: astra });
  const command = app.session.command.bind(app.session);
  vi.spyOn(app.session, "command").mockImplementation(async (body, opts) => {
    if (body.command === "github" && body.action.action === "file_issue") return new Promise(() => {});
    return command(body, opts);
  });
  const mounted = await newOne("issue");
  fileIssue("Orders export drops the last page");
  fireEvent.click(screen.getByRole("button", { name: "File and start" })); await tick();
  mounted.unmount();
  restart();
  await newOne("issue");
  expect(screen.getByText("It may be filed already")).toBeTruthy();
  expect(screen.getByLabelText<HTMLInputElement>("Title").value).toBe("Orders export drops the last page");
  // The person decides: file it after all, or let it go.
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "File and start" }).disabled).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "Discard…" })); await tick();
  expect(screen.getByText(/It may be filed on GitHub already/)).toBeTruthy();
});

it("keeps the issue being written through a failed GitHub refresh, and after leaving and coming back", async () => {
  roles({ fix: astra });
  const query = app.session.query.bind(app.session);
  let fail = false;
  vi.spyOn(app.session, "query").mockImplementation(async (q) => {
    if (q.kind === "github_project" && fail) throw new Error("GitHub didn’t answer");
    return query(q);
  });
  const mounted = await newOne("issue");
  fileIssue("Orders export drops the last page", "Seen on accounts with over 10k orders.");
  fail = true;
  window.__sim!.host.emit({ type: "github_changed", project_id: ids.pTide }); await tick();
  expect(screen.getByText(/GitHub didn’t answer/)).toBeTruthy();
  expect(screen.getByLabelText<HTMLInputElement>("Title").value).toBe("Orders export drops the last page");
  fail = false;
  fireEvent.click(screen.getByRole("button", { name: "Try again" })); await tick();
  expect(screen.queryByText(/GitHub didn’t answer/)).toBeNull();
  expect(screen.getByLabelText<HTMLInputElement>("Title").value).toBe("Orders export drops the last page");
  expect(screen.getByLabelText<HTMLTextAreaElement>("Description").value).toBe("Seen on accounts with over 10k orders.");
  // Back and New issue again: it's still there, until it's discarded.
  mounted.unmount();
  await newOne("issue");
  expect(screen.getByLabelText<HTMLInputElement>("Title").value).toBe("Orders export drops the last page");
  expect(screen.getByLabelText<HTMLTextAreaElement>("Description").value).toBe("Seen on accounts with over 10k orders.");
  fireEvent.click(screen.getByRole("button", { name: "Discard…" })); await tick();
  expect(screen.getByLabelText<HTMLInputElement>("Title").value).toBe("Orders export drops the last page");
  fireEvent.click(screen.getByRole("button", { name: "Discard issue" })); await tick();
  expect(screen.getByLabelText<HTMLInputElement>("Title").value).toBe("");
  expect(screen.getByLabelText<HTMLTextAreaElement>("Description").value).toBe("");
  cleanup();
  await newOne("issue");
  expect(screen.getByLabelText<HTMLInputElement>("Title").value).toBe("");
});

it("keeps settings behind the gear: issues per review and what follows a review", async () => {
  await hub();
  fireEvent.click(screen.getByRole("button", { name: "GitHub settings" })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "GitHub settings" }));
  expect(sheet.getByText("main isn’t protected, so nothing stops a push that skips a pull request.")).toBeTruthy();
  fireEvent.change(sheet.getByLabelText("Issues per review"), { target: { value: "21" } });
  expect(sheet.getByText("1 to 20")).toBeTruthy();
  fireEvent.change(sheet.getByLabelText("Issues per review"), { target: { value: "8" } });
  fireEvent.click(sheet.getByRole("button", { name: "Save" })); await tick();
  expect(github().at(-1)).toMatchObject({ action: "set_settings", settings: { workflow: { findings_per_run: 8 } } });
  fireEvent.click(within(sheet.getByRole("radiogroup", { name: "After a review files issues" })).getByRole("radio", { name: "Only file" })); await tick();
  expect(github().at(-1)).toMatchObject({ action: "set_settings", settings: { workflow: { fix_findings: "file_only", findings_per_run: 8 } } });
  fireEvent.click(sheet.getByRole("switch", { name: "Use GitHub for this project" })); await tick();
  expect(github().at(-1)).toMatchObject({ action: "set_settings", settings: { mode: "off" } });
});

it("flips GitHub's choices at once over a slow link, sends each with the ones before, and shows GitHub on until it is off", async () => {
  await hub();
  fireEvent.click(screen.getByRole("button", { name: "GitHub settings" })); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "GitHub settings" }));
  const label = () => sheet.getByRole<HTMLInputElement>("checkbox", { name: /Label what the agent opens/ });
  const onlyFile = () => within(sheet.getByRole("radiogroup", { name: "After a review files issues" })).getByRole("radio", { name: "Only file" });
  const chosen = () => onlyFile().getAttribute("aria-checked") === "true";
  const on = () => sheet.getByRole<HTMLInputElement>("switch", { name: "Use GitHub for this project" });
  const wait = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
  window.__sim!.latency(1500);
  expect(label().checked).toBe(true);
  fireEvent.click(label());
  await wait(100);
  expect(label().checked).toBe(false);
  expect(label().disabled).toBe(false);
  // A second choice while the first is on its way keeps the first.
  fireEvent.click(onlyFile());
  await wait(100);
  expect(chosen()).toBe(true);
  expect(label().checked).toBe(false);
  await wait(8000);
  expect(window.__sim!.host.github.settings.get(ids.pTide)).toMatchObject({ mode: "drafts", label: false, workflow: { fix_findings: "file_only" } });
  expect([label().checked, chosen()]).toEqual([false, true]);

  // Whether GitHub is on shows what is in force until the computer has the change.
  fireEvent.click(on());
  await wait(300);
  expect(on().checked).toBe(true);
  expect(sheet.getByText("Changing…")).toBeTruthy();
  expect(on().disabled).toBe(false);
  await wait(8000);
  expect(on().checked).toBe(false);
  expect(sheet.queryByText("Changing…")).toBeNull();
  expect(screen.getByText("GitHub is off")).toBeTruthy();
  expect(window.__sim!.host.github.settings.get(ids.pTide)).toMatchObject({ mode: "off", label: false, workflow: { fix_findings: "file_only" } });
});

it("puts the agents' pull requests first, with their checks and whether they can merge", async () => {
  view(<GithubList route={{ screen: "github_list", projectId: ids.pTide }} />, { screen: "github_list", projectId: ids.pTide }); await tick();
  const section = (title: string) => within(screen.getByRole("heading", { name: title }).closest("section")!);
  const ours = section("From agents").getByText("#214 Add rate-limit headers to /v1/orders").closest("button")!;
  // The task on it already says its checks pass, so that isn't said twice.
  expect(within(ours).getAllByText("Checks pass")).toHaveLength(1);
  expect(within(ours).getByText("Can merge")).toBeTruthy();
  const theirs = section("Needs you").getByText("#217 Page the orders export").closest("button")!;
  expect(within(theirs).getByText("Checks fail")).toBeTruthy();
  // A draft can't be merged until it's ready, so it says nothing about merging.
  expect(within(theirs).queryByText("Can’t merge yet")).toBeNull();
  expect(screen.queryByRole("heading", { name: "Waiting to merge" })).toBeNull();
});

it("shows what's waiting to merge together, and what holds it", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find((t) => t.task_id === ids.tHeaders)!;
  const merging = { ...task, github: { ...task.github!, stage: "merging" as const }, revision: task.revision + 1 };
  host.tasks = host.tasks.map((t) => (t.task_id === task.task_id ? merging : t));
  host.emit({ type: "task_upserted", task: merging });
  await tick();
  view(<GithubList route={{ screen: "github_list", projectId: ids.pTide }} />, { screen: "github_list", projectId: ids.pTide }); await tick();
  const waiting = within(screen.getByRole("heading", { name: "Waiting to merge" }).closest("section")!);
  expect(waiting.getByText("#214 Add rate-limit headers to /v1/orders")).toBeTruthy();
  expect(waiting.getByText(/^1 is waiting to merge\./)).toBeTruthy();
  expect(screen.queryByRole("heading", { name: "From agents" })).toBeNull();
});

it("counts each of the agents' open pull requests once, among the project's doors", async () => {
  const host = window.__sim!.host;
  const task = host.tasks.find((t) => t.task_id === ids.tHeaders)!;
  const doors = () => {
    view(<GithubDoors project={app.session.state.get().projects[ids.pTide]!} />, { screen: "work" });
    return tick().then(() => screen.getByRole("button", { name: /^Issues & pull requests/ }).textContent);
  };
  const merge = (t: Task) => {
    const merged = { ...t, github: { ...t.github!, pr: { ...t.github!.pr!, state: "merged" as const }, stage: "merged" as const }, revision: t.revision + 1 };
    host.tasks = host.tasks.map((x) => (x.task_id === t.task_id ? merged : x));
    host.emit({ type: "task_upserted", task: merged });
  };
  // Two of them wait for the person: one needs approval, one's review stopped.
  expect(await doors()).toContain("3 of the agents’ pull requests are open · 2 need you");
  cleanup();
  for (const id of [ids.tSigning, ids.tExport]) merge(host.tasks.find((t) => t.task_id === id)!);
  expect(await doors()).toContain("1 of the agents’ pull requests is open");
  cleanup();
  // A check on it speaks of the same pull request.
  const check = { ...task, task_id: "0192f7a1-0000-7000-8000-00000000ce01", title: `Check #${task.github!.pr!.number}`, github: { ...task.github!, stage: undefined } };
  host.tasks = [...host.tasks, check];
  host.emit({ type: "task_upserted", task: check });
  expect(await doors()).toContain("1 of the agents’ pull requests is open");
  cleanup();
  // Merged on GitHub, neither is open.
  for (const t of [task, check]) merge(t);
  expect(await doors()).toContain("Open issues and the agents’ pull requests");
});

it("says the merge line in words", () => {
  expect(lineWords({ numbers: [214], running: 0, nextUp: 0, paused: false }))
    .toBe("1 is waiting to merge. Nothing is running or in Next up, so it’s tested and merged now.");
  expect(lineWords({ numbers: [214, 220], running: 0, nextUp: 0, paused: false }))
    .toBe("2 are waiting to merge. Nothing is running or in Next up, so they’re tested together and merged now.");
  expect(lineWords({ numbers: [214, 220, 223], running: 1, nextUp: 2, paused: true }))
    .toBe("3 are waiting to merge. They merge together once nothing is running and Next up is empty: 1 running, 2 in Next up, which is paused.");
  expect(lineWords({ numbers: [214], running: 0, nextUp: 1, paused: false }))
    .toBe("1 is waiting to merge. It merges once nothing is running and Next up is empty: 1 in Next up.");
});

it("shows a new pull request in the chat, and drops it from there", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Add a retry budget header", agent: astra, mode: "work", review: { ...freshReview(), github: { type: "new_pr" } } });
  const project = app.session.state.get().projects[ids.pTide]!;
  view(<Composer project={project} task={null} run={null} harness={null} agent={astra} />); await tick();
  expect(screen.getByText("Pull request")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Don’t open a pull request" })); await tick();
  expect(screen.queryByText("Pull request")).toBeNull();
  expect(app.ui.draft(ids.pTide, null)?.review?.github).toBeNull();
});

it("lists pull requests on their own, and takes one over through Review", async () => {
  view(<GithubList route={{ screen: "github_list", projectId: ids.pTide }} />, { screen: "github_list", projectId: ids.pTide }); await tick();
  expect(screen.queryByText(/#219/)).toBeNull();
  fireEvent.click(screen.getByText("#214 Add rate-limit headers to /v1/orders")); await tick();
  const sheet = within(screen.getByRole("dialog", { name: "#214 Add rate-limit headers to /v1/orders" }));
  fireEvent.click(sheet.getByRole("button", { name: "Take it over" })); await tick();
  expect(app.ui.draft(ids.pTide, null)).toMatchObject({ mode: "work", review: { github: { type: "pr", number: 214 } } });
});

it("merges from the list, or says why it can't and offers to resolve its conflicts", async () => {
  const confirm = vi.spyOn(app.platform.appLock!, "confirm");
  const pr = window.__sim!.host.github.prs.get(214)!;
  pr.mergeable = false; pr.blocked = "It conflicts with `main`."; pr.summary.conflict = true;
  view(<GithubList route={{ screen: "github_list", projectId: ids.pTide }} />, { screen: "github_list", projectId: ids.pTide }); await tick();
  fireEvent.click(screen.getByText("#214 Add rate-limit headers to /v1/orders")); await tick();
  let sheet = within(screen.getByRole("dialog", { name: "#214 Add rate-limit headers to /v1/orders" }));
  expect(sheet.getByRole<HTMLButtonElement>("button", { name: "Squash and merge" }).disabled).toBe(true);
  expect(sheet.getByText("main", { selector: "code" })).toBeTruthy();
  expect(sheet.getByText(/Resolve conflicts has an agent bring it up to date/)).toBeTruthy();
  expect(sheet.getByRole("button", { name: "Take it over" })).toBeTruthy();
  fireEvent.click(sheet.getByRole("button", { name: "Resolve conflicts" })); await tick();
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(app.ui.draft(ids.pTide, null)).toMatchObject({
    text: "Bring pull request #214 up to date with main and resolve its conflicts.", mode: "work", review: { github: { type: "pr", number: 214 } },
  });

  pr.mergeable = true; delete pr.blocked; pr.summary.conflict = false;
  fireEvent.click(screen.getByText("#214 Add rate-limit headers to /v1/orders")); await tick();
  sheet = within(screen.getByRole("dialog", { name: "#214 Add rate-limit headers to /v1/orders" }));
  expect(sheet.queryByText("It can’t merge yet")).toBeNull();
  fireEvent.click(sheet.getByRole("button", { name: "Squash and merge" })); await tick();
  expect(confirm).toHaveBeenLastCalledWith("Merge pull request #214");
  expect(github().at(-1)).toMatchObject({ action: "merge_pr", repo: "samrivera/tidewater-api", number: 214, expected_head: pr.summary.head, method: "squash" });
  expect(screen.getByText("Pull request #214 merged")).toBeTruthy();
});

it("says what each picked row showed as needing approval", async () => {
  const prs = window.__sim!.host.github.prs;
  view(<GithubList route={{ screen: "github_list", projectId: ids.pTide }} />, { screen: "github_list", projectId: ids.pTide }); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Merge several together" })); await tick();
  fireEvent.click(screen.getByRole("checkbox", { name: /^#218 / })); await tick();
  fireEvent.click(screen.getByRole("checkbox", { name: /^#214 / })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Merge 2 together" })); await tick();
  const shown = app.session.state.get().tasks[ids.tSigning]!.github!.approval!.areas;
  expect(shown.length).toBeGreaterThan(0);
  expect(github().at(-1)).toMatchObject({
    action: "merge_together",
    prs: [{ number: 214, head: prs.get(214)!.summary.head, approval_shown: [] }, { number: 218, head: prs.get(218)!.summary.head, approval_shown: shown }],
  });
});

it("picks pull requests to merge together behind the unlock, then shows how each got on", async () => {
  const confirm = vi.spyOn(app.platform.appLock!, "confirm");
  const prs = window.__sim!.host.github.prs;
  view(<GithubList route={{ screen: "github_list", projectId: ids.pTide }} />, { screen: "github_list", projectId: ids.pTide }); await tick();
  expect(screen.queryByRole("checkbox")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Merge several together" })); await tick();
  const merge = () => screen.getByRole<HTMLButtonElement>("button", { name: /^Merge (\d+ )?together$/ });
  expect(merge().disabled).toBe(true);
  expect(screen.getByText(/^Pick two or more\./)).toBeTruthy();
  // A tap picks it rather than opening it.
  fireEvent.click(screen.getByRole("checkbox", { name: /^#214 / })); await tick();
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.getByRole("checkbox", { name: /^#214 / }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(screen.getByRole("checkbox", { name: /^#217 / })); await tick();
  expect(merge().textContent).toBe("Merge 2 together");
  fireEvent.click(merge()); await tick();
  expect(confirm).toHaveBeenLastCalledWith("Merge 2 pull requests together");
  expect(github().at(-1)).toEqual({
    action: "merge_together", project_id: ids.pTide, repo: "samrivera/tidewater-api",
    // Neither row showed anything needing approval, so picking approves nothing.
    prs: [{ number: 214, head: prs.get(214)!.summary.head, approval_shown: [] }, { number: 217, head: prs.get(217)!.summary.head, approval_shown: [] }],
  });
  await tick();
  expect(screen.queryByRole("checkbox")).toBeNull();
  const section = () => within(screen.getByRole("heading", { name: "Merging together" }).closest("section")!);
  expect(section().getAllByText("Testing")).toHaveLength(2);
  expect(section().getByText(/^Testing them together with main, then merging each in turn\. 0 of 2 merged so far\./)).toBeTruthy();
  expect(section().getByRole("button", { name: "See the merge check" })).toBeTruthy();
  // Not offered again while it runs.
  expect(screen.queryByRole("button", { name: "Merge several together" })).toBeNull();

  await act(async () => { await vi.advanceTimersByTimeAsync(6_000); }); await tick();
  expect(section().getByText("1 of 2 merged. The rest were left out.")).toBeTruthy();
  const out = section().getByText("#217 Page the orders export").closest<HTMLElement>(".row")!;
  expect(within(out).getByText("Left out")).toBeTruthy();
  expect(within(out).getByText("test", { selector: "code" })).toBeTruthy();
  expect(section().getByText("Merged")).toBeTruthy();
  expect(section().queryByRole("button", { name: "See the merge check" })).toBeNull();
  fireEvent.click(section().getByRole("button", { name: "Done" })); await tick();
  expect(github().at(-1)).toEqual({ action: "stop_together", project_id: ids.pTide });
  await tick();
  expect(screen.queryByRole("heading", { name: "Merging together" })).toBeNull();
});

it("stops merging together, and leaves what hadn't merged", async () => {
  view(<GithubList route={{ screen: "github_list", projectId: ids.pTide }} />, { screen: "github_list", projectId: ids.pTide }); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Merge several together" })); await tick();
  fireEvent.click(screen.getByRole("checkbox", { name: /^#214 / }));
  fireEvent.click(screen.getByRole("checkbox", { name: /^#217 / })); await tick();
  // Cancel picks nothing and sends nothing.
  fireEvent.click(screen.getByRole("button", { name: "Cancel" })); await tick();
  expect(screen.queryByRole("checkbox")).toBeNull();
  expect(github()).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: "Merge several together" })); await tick();
  fireEvent.click(screen.getByRole("checkbox", { name: /^#214 / }));
  fireEvent.click(screen.getByRole("checkbox", { name: /^#217 / })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Merge 2 together" })); await tick(); await tick();
  const section = within(screen.getByRole("heading", { name: "Merging together" }).closest("section")!);
  fireEvent.click(section.getByRole("button", { name: "Stop" })); await tick(); await tick();
  expect(github().at(-1)).toEqual({ action: "stop_together", project_id: ids.pTide });
  expect(section.getByText("You stopped it. 0 of 2 merged.")).toBeTruthy();
  expect(section.getAllByText("You stopped it.")).toHaveLength(2);
  expect(screen.getByRole("button", { name: "Merge several together" })).toBeTruthy();
});

it("says how merging together went in words", () => {
  const pr = (number: number, state: "waiting" | "testing" | "merged" | "left_out") => ({ number, title: "x", state });
  const t = (states: Parameters<typeof pr>[1][], rest = {}) => ({ prs: states.map((s, i) => pr(i + 1, s)), started_at: 0, stopped: false, ...rest });
  expect(togetherWords(t(["merged", "testing", "waiting"]), "main"))
    .toBe("Testing them together with main, then merging each in turn. 1 of 3 merged so far.");
  expect(togetherWords(t(["merged", "merged"], { ended_at: 1 }), "main")).toBe("Both merged.");
  expect(togetherWords(t(["merged", "merged", "merged"], { ended_at: 1 }), "main")).toBe("All 3 merged.");
  expect(togetherWords(t(["left_out", "left_out"], { ended_at: 1 }), "main")).toBe("None merged.");
  expect(togetherWords(t(["merged", "left_out"], { ended_at: 1, stopped: true }), "main")).toBe("You stopped it. 1 of 2 merged.");
  expect(togetherWords(t(["testing", "left_out"], { stopped: true }), "main"))
    .toBe("You stopped it. Checking the merge already sent to GitHub.");
});
