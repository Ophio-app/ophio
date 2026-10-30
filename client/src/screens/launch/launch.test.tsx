// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore, type Route } from "../../app/ui";
import { ONE_OFF_DRAFT, SOURCE_LABEL, entryAgent, localDate, oneOffFolder, projectSource, setDraftStart, skillHarness, slug, startedLabel, type LaunchStart } from "../../app/launchers";
import { SETUP_AGENTS, SETUP_SKILLS, catalog } from "../../dev/sim/launchers";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import { ProjectSheet as Projects } from "../../shell/ProjectSheet";
import type { CatalogEntry } from "../../protocol/types";
import Review from "../talk/Review";
import Talk from "../talk/Talk";
import { TaskAgentChip, TaskAgentSheet } from "../talk/TaskAgent";
import ProjectSheet from "../work/ProjectSheet";
import Library from "../library/Library";

let app: App;
const tick = async () => act(async () => { await vi.advanceTimersByTimeAsync(900); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  // The simulator hashes agent files; native digests don't settle under fake time.
  vi.stubGlobal("crypto", { getRandomValues: crypto.getRandomValues.bind(crypto), randomUUID: crypto.randomUUID.bind(crypto) });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick();
  // Free the project's writer so a new Work task can start now.
  const host = window.__sim!.host;
  const project = { ...session.state.get().projects[ids.pTide]!, active_writer: null };
  host.projects = host.projects.map(p => p.project_id === project.project_id ? project : p);
  host.emit({ type: "project_upserted", project });
  await tick();
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

const wrap = (route: Route, node: React.ReactNode, tab: "talk" | "library" = "talk") =>
  render(<AppProvider app={app}><RouteProvider value={{ tab, depth: 1, active: true, route }}>{node}</RouteProvider></AppProvider>);
const review = (projectId: string) => {
  const route = { screen: "review", projectId, taskId: null, intent: "new_task" } as const;
  return wrap(route, <Review route={route} />);
};
const created = () => Object.values(app.session.commands.get()).find(c => c.body.command === "create_task")?.body;
const astra = { harness: "codex" as const, model: "gpt-6-astra", effort: "high" };

async function videoProducer(): Promise<LaunchStart> {
  const read = app.session.query({ kind: "catalog", project_id: null });
  await tick();
  const entry = (await read).catalog.entries.find(e => e.name === "Video producer")!;
  return { start: { type: "agent", source: entry.source, id: entry.id, digest: entry.digest }, name: entry.name, source: entry.source, hostPath: entry.host_path };
}

it("lists the computer's agents and skills in Library and opens Review set to start from one", async () => {
  app.ui.selectProject(ids.pHome);
  wrap({ screen: "library" }, <Library route={{ screen: "library" }} />, "library"); await tick();
  // The count is the rows listed, Scratch pad among them.
  const agents = screen.getAllByRole("button", { name: /^About / }).length + 1;
  expect(screen.getByRole("button", { name: /^Scratch pad/ })).toBeTruthy();
  expect(screen.getByRole("radio", { name: `Agents ${agents}` })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "About Video producer" })); await tick();
  expect(screen.getByText("Agent · Yours")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start from this" })); await tick();
  const draft = app.ui.draft(ids.pHome, null)!;
  expect(draft.review?.start).toMatchObject({ name: "Video producer", source: "ophio", start: { type: "agent", id: "video-producer" } });
  // It asks for Claude Code and its newest Opus, at high effort.
  expect(draft.agent).toMatchObject({ harness: "claude_code", effort: "high" });
  expect(draft.agent?.model).toMatch(/opus/);
  expect(app.ui.get().stacks[app.ui.get().tab].at(-1)).toEqual({ screen: "review", projectId: ids.pHome, taskId: null, intent: "new_task" });

  fireEvent.click(screen.getByRole("radio", { name: /^Skills/ }));
  expect(screen.getByRole("button", { name: "Start from imagegen" })).toBeTruthy();
  // A project's own are listed under that project.
  expect(screen.queryByRole("button", { name: "Start from migrations" })).toBeNull();
});

it("narrows Library to a project's own agents and skills, then Øphio's, and makes new agents at Home", async () => {
  app.ui.selectProject(ids.pTide);
  wrap({ screen: "library" }, <Library route={{ screen: "library" }} />, "library"); await tick();
  const about = () => screen.getAllByRole("button", { name: /^About / }).map(b => b.getAttribute("aria-label"));
  // The project's own come first; Øphio's follow, and nobody else's.
  expect(about()[0]).toBe("About api-reviewer");
  expect(about()).toContain("About Video producer");
  expect(screen.queryByRole("button", { name: /^Scratch pad/ })).toBeNull();
  expect(screen.queryByRole("button", { name: /New agent/ })).toBeNull();
  fireEvent.click(screen.getByRole("radio", { name: /^Skills/ }));
  expect(about()[0]).toBe("About migrations");
  expect(screen.getByRole("button", { name: "Start from setup-phone" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Start from imagegen" })).toBeNull();

  // A project with none of its own still has Øphio's.
  act(() => app.ui.selectProject(ids.pNotes)); await tick();
  expect(screen.queryByRole("button", { name: "Start from migrations" })).toBeNull();
  expect(screen.getByRole("button", { name: "Start from setup-phone" })).toBeTruthy();
  expect(screen.queryByText(/has no skills of its own/)).toBeNull();
});

it("makes a new agent at Home and opens its file once the computer has written it", async () => {
  app.ui.selectProject(ids.pHome);
  wrap({ screen: "library" }, <Library route={{ screen: "library" }} />, "library"); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^New agent/ })); await tick();
  const sheet = screen.getByRole("dialog", { name: /New agent/ });
  fireEvent.change(within(sheet).getByRole("textbox", { name: "Name" }), { target: { value: "Release notes" } });
  fireEvent.click(within(sheet).getByRole("button", { name: "Make and write it" })); await tick();
  // The computer says what it made after accepting, not with the acceptance.
  expect(app.ui.get().stacks[app.ui.get().tab].at(-1)).toEqual({ screen: "file", rootId: ids.rootOphio, path: "Agents/release-notes.md" });
  expect(window.__sim!.host.files[ids.rootOphio]!["Agents/release-notes.md"]).toMatch(/^---\nname: release-notes/);
});

it("copies one of Øphio's skills to edit, and keeps that copy when a newer version ships", async () => {
  const host = window.__sim!.host;
  const path = "Skills/setup-phone/SKILL.md";
  const top = () => app.ui.get().stacks[app.ui.get().tab].at(-1);
  const about = async () => {
    fireEvent.click(screen.getByRole("radio", { name: /^Skills/ }));
    fireEvent.click(screen.getByRole("button", { name: "About setup-phone" })); await tick();
    return screen.getByRole("dialog", { name: /setup-phone/ });
  };
  const route = { screen: "library" } as const;
  const library = (active: boolean) => <AppProvider app={app}><RouteProvider value={{ tab: "library", depth: 1, active, route }}><Library route={route} /></RouteProvider></AppProvider>;
  const { rerender } = render(library(true)); await tick();
  let sheet = await about();
  expect(within(sheet).getByText("Skill · Built into Øphio")).toBeTruthy();
  expect(within(sheet).getByText(/Øphio’s updates never write over it/)).toBeTruthy();
  fireEvent.click(within(sheet).getByRole("button", { name: "Customize" })); await tick();
  // The copy opens to edit, made from Øphio's as it is now.
  expect(top()).toEqual({ screen: "file", rootId: ids.rootOphio, path, edit: true });
  expect(host.files[ids.rootOphio]![path]).toBe(SETUP_SKILLS["setup-phone"]);

  // Edited, it is theirs; a newer version of Øphio's is offered, never written over it.
  const mine = "---\nname: setup-phone\ndescription: My own way\n---\nMine.\n";
  host.files[ids.rootOphio]![path] = mine;
  host.skillBases["setup-phone"] = "an older version";
  // Back from the file, Library reads the list again.
  rerender(library(false)); await tick();
  rerender(library(true)); await tick();
  sheet = await about();
  expect(within(sheet).getByText("Skill · Built into Øphio · your copy")).toBeTruthy();
  expect(within(sheet).getByText("My own way")).toBeTruthy();
  expect(within(sheet).queryByRole("button", { name: "Customize" })).toBeNull();
  expect(within(sheet).getByRole("button", { name: "Edit its file" })).toBeTruthy();
  expect(within(sheet).getByText("Øphio has a newer version of this skill")).toBeTruthy();
  fireEvent.click(within(sheet).getByRole("button", { name: "Keep mine" })); await tick(); await tick();
  expect(host.files[ids.rootOphio]![path]).toBe(mine);
  sheet = await about();
  expect(within(sheet).queryByText("Øphio has a newer version of this skill")).toBeNull();

  // Taking the new version puts theirs in Recently deleted.
  cleanup();
  host.skillBases["setup-phone"] = "an older version";
  wrap({ screen: "library" }, <Library route={{ screen: "library" }} />, "library"); await tick();
  sheet = await about();
  // The simulator's trash hashes with SubtleCrypto, which is stubbed out here.
  const putAside = vi.spyOn(host.libraryWorkflows, "deleted").mockResolvedValue({ kind: "library_deleted" } as never);
  fireEvent.click(within(sheet).getByRole("button", { name: "Use the new version" })); await tick();
  expect(host.files[ids.rootOphio]![path]).toBe(SETUP_SKILLS["setup-phone"]);
  expect(putAside).toHaveBeenCalledWith(ids.rootOphio, path, mine);
});

it("copies one of Øphio's agents to edit, listed once as that copy", async () => {
  const host = window.__sim!.host;
  const path = "Agents/bug-hunter.md";
  wrap({ screen: "library" }, <Library route={{ screen: "library" }} />, "library"); await tick();
  fireEvent.click(screen.getByRole("button", { name: "About Bug hunter" })); await tick();
  const sheet = screen.getByRole("dialog", { name: /Bug hunter/ });
  expect(within(sheet).getByText("Agent · Built into Øphio")).toBeTruthy();
  expect(within(sheet).getByText(/your own copy in Øphio’s agents folder/)).toBeTruthy();
  fireEvent.click(within(sheet).getByRole("button", { name: "Customize" })); await tick();
  expect(app.ui.get().stacks[app.ui.get().tab].at(-1)).toEqual({ screen: "file", rootId: ids.rootOphio, path, edit: true });
  expect(host.files[ids.rootOphio]![path]).toBe(SETUP_AGENTS["bug-hunter"]);
  const listed = (await catalog(host, async (text) => text)).entries.filter((e) => e.id === "bug-hunter");
  expect(listed.map((e) => [e.source, Boolean(e.copy)])).toEqual([["ophio_setup", true]]);
});

it("starts a task from an agent, and the task says so", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Make the launch video", agent: astra, mode: "work" });
  setDraftStart(app.ui, ids.pTide, await videoProducer());
  review(ids.pTide); await tick();
  expect(screen.getByText("Starts from Agent: Video producer")).toBeTruthy();
  expect(screen.getByText("Its instructions go to Codex with your request.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).toMatchObject({ command: "create_task", project_id: ids.pTide, start: { type: "agent", source: "ophio", id: "video-producer" } });
  expect(created()).not.toHaveProperty("place");
  const task = Object.values(app.session.state.get().tasks).find(t => t.title === "Make the launch video")!;
  expect(task.start).toEqual({ type: "agent", name: "Video producer" });
});

it("can drop what a draft starts from before it's sent", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Make the launch video", agent: astra, mode: "work" });
  setDraftStart(app.ui, ids.pTide, await videoProducer());
  review(ids.pTide); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Don’t start from Video producer" }));
  expect(screen.queryByText(/Starts from/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).not.toHaveProperty("start");
});

it("starts a Next up task as the task it follows did, unless that is dropped", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Make the launch video", agent: astra, mode: "work" });
  setDraftStart(app.ui, ids.pTide, await videoProducer());
  review(ids.pTide); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  cleanup();
  const source = Object.values(app.session.state.get().tasks).find(t => t.title === "Make the launch video")!;
  const sent = (prompt: string) => Object.values(app.session.commands.get()).map(c => c.body).find(b => b.command === "create_task" && b.prompt === prompt);
  const nextUp = async (text: string, drop: boolean) => {
    app.ui.saveDraft(ids.pTide, source.task_id, { text });
    const route = { screen: "review", projectId: ids.pTide, taskId: source.task_id, intent: "next_up" } as const;
    wrap(route, <Review route={route} />); await tick();
    expect(screen.getByText("Starts from Agent: Video producer")).toBeTruthy();
    expect(screen.getByText(/^Kept from the task it follows, as that task was given it\. Its instructions go to/)).toBeTruthy();
    if (drop) fireEvent.click(screen.getByRole("button", { name: "Don’t start from Video producer" }));
    fireEvent.click(screen.getByRole("button", { name: "Add to Next up" })); await tick();
    cleanup();
  };
  await nextUp("Cut a shorter one", false);
  expect(sent("Cut a shorter one")).toMatchObject({ start: { type: "same_as", task_id: source.task_id } });
  expect(Object.values(app.session.state.get().tasks).find(t => t.title === "Cut a shorter one")?.start).toEqual({ type: "agent", name: "Video producer" });
  await nextUp("Write the captions", true);
  expect(sent("Write the captions")).not.toHaveProperty("start");
});

it("changes what a Next up task starts from, and chooses again after it's removed", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Make the launch video", agent: astra, mode: "work" });
  setDraftStart(app.ui, ids.pTide, await videoProducer());
  review(ids.pTide); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  cleanup();
  const source = Object.values(app.session.state.get().tasks).find(t => t.title === "Make the launch video")!;
  app.ui.saveDraft(ids.pTide, source.task_id, { text: "Draw the thumbnails" });
  const route = { screen: "review", projectId: ids.pTide, taskId: source.task_id, intent: "next_up" } as const;
  wrap(route, <Review route={route} />); await tick();
  fireEvent.click(screen.getByRole("button", { name: /^Starts from Agent: Video producer/ })); await tick();
  let sheet = screen.getByRole("dialog", { name: /Start from/ });
  fireEvent.click(within(sheet).getByRole("radio", { name: "Skills" }));
  fireEvent.click(within(sheet).getByRole("button", { name: /imagegen/ })); await tick();
  expect(app.ui.draft(ids.pTide, source.task_id)?.review?.start).toMatchObject({ name: "imagegen", start: { type: "skill" } });
  expect(screen.getByText("Starts from Skill: imagegen")).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Don’t start from imagegen" }));
  expect(screen.queryByText(/^Starts from/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Start from" })); await tick();
  sheet = screen.getByRole("dialog", { name: /Start from/ });
  fireEvent.click(within(sheet).getByRole("radio", { name: "Scratch" }));
  fireEvent.click(within(sheet).getByRole("button", { name: /Scratch pad/ }));
  expect(screen.getByText("Starts from Scratch pad")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Add to Next up" })); await tick();
  const sent = Object.values(app.session.commands.get()).map(c => c.body).find(b => b.command === "create_task" && b.prompt === "Draw the thumbnails");
  expect(sent).toMatchObject({ start: { type: "scratch" } });
});

it("starts a task on its own in a new folder, listed under One-offs", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Make a birthday card for Ana", agent: astra, mode: "work" });
  review(ids.pTide); await tick();
  fireEvent.click(screen.getByRole("radio", { name: "On its own" })); await tick();
  expect(app.ui.draft(ids.pTide, null)?.review?.place).toBe("one_off");
  expect(screen.getByText(`A new folder, ${localDate()}-make-a-birthday-card-for-ana`)).toBeTruthy();
  expect(document.querySelector(".review__mono")?.textContent).toBe("/home/sam/Ophio");
  // No branch and no Next up: the folder is new.
  expect(screen.queryByRole("button", { name: /^Branch:/ })).toBeNull();
  expect(screen.getByText("Starts now. A task on its own has no Next up to wait in.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  const body = created()!;
  expect(body).toMatchObject({ command: "create_task", placement: { mode: "start_now" }, place: { type: "one_off", name: "Make a birthday card for Ana", date: localDate() } });
  expect(body).not.toHaveProperty("branch");
  const projectId = (body as { project_id: string }).project_id;
  expect(projectId).not.toBe(ids.pTide);
  const project = app.session.state.get().projects[projectId]!;
  expect(project).toMatchObject({ one_off: true, name: "Make a birthday card for Ana", host_path: `/home/sam/Ophio/${localDate()}-make-a-birthday-card-for-ana` });
  expect(Object.values(app.session.state.get().tasks).find(t => t.title === "Make a birthday card for Ana")?.project_id).toBe(projectId);
});

it("starts a draft made with no project on its own, from what it was set to", async () => {
  app.ui.saveDraft(ONE_OFF_DRAFT, null, { text: "Storyboard the teaser", agent: astra, mode: "work" });
  setDraftStart(app.ui, ONE_OFF_DRAFT, await videoProducer());
  review(ONE_OFF_DRAFT); await tick();
  expect(screen.queryByRole("radio", { name: "On its own" })).toBeNull();
  expect(screen.getByText(/^A new folder, /)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Start task" })); await tick();
  expect(created()).toMatchObject({ place: { type: "one_off", name: "Storyboard the teaser" }, start: { type: "agent", id: "video-producer" } });
  const task = Object.values(app.session.state.get().tasks).find(t => t.title === "Storyboard the teaser")!;
  expect(app.session.state.get().projects[task.project_id]?.one_off).toBe(true);
  expect(task.start).toEqual({ type: "agent", name: "Video producer" });
});

it("won't start a project's own agent on its own", async () => {
  app.ui.saveDraft(ids.pTide, null, { text: "Review the API", agent: astra, mode: "work" });
  setDraftStart(app.ui, ids.pTide, { start: { type: "agent", source: "claude_project", id: "api-reviewer", digest: "0".repeat(64) }, name: "API reviewer", source: "claude_project" });
  app.ui.saveDraft(ids.pTide, null, { review: { ...app.ui.draft(ids.pTide, null)!.review!, place: "one_off" } });
  review(ids.pTide); await tick();
  expect(screen.getByText("“API reviewer” is kept in tidewater-api’s own folder, so it can start only there.")).toBeTruthy();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Start task" }).disabled).toBe(true);
});

it("chooses what a new task starts from in Chat", async () => {
  app.ui.selectTask(ids.pTide, null);
  app.ui.saveDraft(ids.pTide, null, { text: "Make the launch video", agent: astra, mode: "work" });
  wrap({ screen: "talk" }, <Talk route={{ screen: "talk" }} />); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Start from" })); await tick();
  const sheet = screen.getByRole("dialog", { name: /Start from/ });
  fireEvent.click(within(sheet).getByRole("button", { name: /Video producer/ })); await tick();
  expect(app.ui.draft(ids.pTide, null)?.review?.start?.name).toBe("Video producer");
  expect(screen.getByRole("button", { name: "Agent: Video producer" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Don’t start from Video producer" }));
  expect(app.ui.draft(ids.pTide, null)?.review?.start).toBeNull();
  expect(screen.getByRole("button", { name: "Start from" })).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Start from" })); await tick();
  fireEvent.click(within(screen.getByRole("dialog", { name: /Start from/ })).getByRole("radio", { name: "Scratch" }));
  fireEvent.click(screen.getByRole("button", { name: /Scratch pad/ }));
  expect(app.ui.draft(ids.pTide, null)?.review?.start?.start).toEqual({ type: "scratch" });
  expect(screen.getByRole("button", { name: /^Scratch pad/ })).toBeTruthy();
});

it("shows what a task started from beside its agent", () => {
  const agent = { label: "Claude Code · Opus", status: "", flags: "Ask for approval", risk: null, launch: { harness: "claude_code" as const, policy: "standard" as const, mode: "work" as const }, branch: null, started: "Skill: imagegen" };
  render(<AppProvider app={app}><TaskAgentChip agent={agent} onClick={() => {}} /><TaskAgentSheet open onClose={() => {}} agent={agent} /></AppProvider>);
  expect(document.querySelector(".composer__agent")?.textContent).toBe("Agent: Claude Code · Opus · Skill: imagegen");
  expect(screen.getByText("Started from")).toBeTruthy();
  expect(screen.getByText(/A task keeps its agent, permission setting and branch, and what it started from\./)).toBeTruthy();
});

it("lists one-offs apart from the projects", async () => {
  render(<AppProvider app={app}><Projects open onClose={() => {}} /></AppProvider>); await tick();
  const oneOffs = screen.getByRole("heading", { name: "One-offs" }).closest("section")!;
  expect(within(oneOffs).getByText("Garden zine")).toBeTruthy();
  expect(within(oneOffs).queryByText("tidewater-api")).toBeNull();
  expect(screen.getByText("tidewater-api")).toBeTruthy();
});

it("opens a one-off's folder and makes it a project", async () => {
  const zine = app.session.state.get().projects[ids.pZine]!;
  const closed = vi.fn();
  const { rerender } = render(<AppProvider app={app}><ProjectSheet project={zine} onClose={closed} /></AppProvider>); await tick();
  fireEvent.click(screen.getByRole("button", { name: /Open its folder/ }));
  expect(closed).toHaveBeenCalled();
  expect(app.ui.get().stacks.library.at(-1)).toEqual({ screen: "folder", rootId: ids.rootOphio, path: "2026-10-01-garden-zine" });
  fireEvent.click(screen.getByRole("button", { name: "Make it a project" })); await tick();
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "promote_one_off")).toBe(true);
  const promoted = app.session.state.get().projects[ids.pZine]!;
  expect(promoted.one_off).toBe(false);
  rerender(<AppProvider app={app}><ProjectSheet project={promoted} onClose={closed} /></AppProvider>);
  expect(screen.queryByRole("button", { name: "Make it a project" })).toBeNull();
});

it("deletes a one-off from the projects list, once asked", async () => {
  render(<AppProvider app={app}><Projects open onClose={() => {}} /></AppProvider>); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Delete Garden zine" })); await tick();
  expect(screen.getByText(/Its folder and everything in it are deleted/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Keep it" })); await tick();
  expect(app.session.state.get().projects[ids.pZine]).toBeTruthy();
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "delete_one_off")).toBe(false);

  fireEvent.click(screen.getByRole("button", { name: "Delete Garden zine" })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Delete Garden zine" })); await tick();
  expect(Object.values(app.session.commands.get()).some(c => c.body.command === "delete_one_off" && c.body.project_id === ids.pZine)).toBe(true);
  expect(app.session.state.get().projects[ids.pZine]).toBeUndefined();
  expect(Object.values(app.session.state.get().tasks).filter(t => t.project_id === ids.pZine).every(t => t.state === "archived")).toBe(true);
  expect(screen.queryByRole("heading", { name: "One-offs" })).toBeNull();
  expect(screen.getByText("tidewater-api")).toBeTruthy();
});

it("deletes a one-off from its settings, and offers it only for a one-off", async () => {
  const state = app.session.state.get();
  const closed = vi.fn();
  const { rerender } = render(<AppProvider app={app}><ProjectSheet project={state.projects[ids.pTide]!} onClose={closed} /></AppProvider>); await tick();
  expect(screen.queryByRole("button", { name: "Delete it…" })).toBeNull();
  rerender(<AppProvider app={app}><ProjectSheet project={state.projects[ids.pZine]!} onClose={closed} /></AppProvider>); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Delete it…" })); await tick();
  expect(screen.getByRole("dialog", { name: /Delete one-off/ })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Delete Garden zine" })); await tick();
  expect(closed).toHaveBeenCalled();
  expect(app.session.state.get().projects[ids.pZine]).toBeUndefined();
  expect(app.ui.get().projectId).not.toBe(ids.pZine);
});

it("names folders, labels and agents as the computer does", () => {
  expect(slug("Make a birthday card — for Ana!")).toBe("make-a-birthday-card-for-ana");
  expect(slug("***")).toBe("task");
  expect(slug("a".repeat(60))).toHaveLength(48);
  expect(startedLabel({ type: "scratch" })).toBe("Scratch pad");
  expect(startedLabel({ type: "skill", name: "imagegen" })).toBe("Skill: imagegen");
  expect(startedLabel(null)).toBeNull();
  expect(localDate(new Date(2026, 0, 5, 23, 30))).toBe("2026-01-05");
  expect(oneOffFolder({ ...app.session.state.get().projects[ids.pZine]! }, [{ root_id: "r", label: "Ophio", host_path: "/home/sam/Ophio/", kind: "notes", writable: true, project_id: null, harness: null }]))
    .toEqual({ rootId: "r", path: "2026-10-01-garden-zine" });
  const harnesses = app.session.state.get().harnesses;
  const entry = { kind: "agent", source: "claude_user", id: "x", name: "x", description: null, digest: "", host_path: "", library: null, harness: "claude_code", model: "no-such-model", effort: "not-an-effort", launch_policy: null } as CatalogEntry;
  // What this computer doesn't offer is left to the agent's default.
  expect(entryAgent(entry, harnesses)).toEqual({ harness: "claude_code", model: null, effort: null });
  expect(entryAgent({ ...entry, harness: null }, harnesses)).toBeNull();
  // Øphio's own setup skills suit any agent and any task.
  expect(SOURCE_LABEL.ophio_setup).toBe("Built into Øphio");
  expect(skillHarness("ophio_setup")).toBeNull();
  expect(projectSource("ophio_setup")).toBe(false);
});
