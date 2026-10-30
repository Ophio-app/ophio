// SPDX-License-Identifier: Apache-2.0
// Disable in Øphio (D-071) in Library: the choice, the Disabled section,
// taps that show at once and go back when refused, and the line that says
// Library is still checking the computer.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { resetDisabled, kindLabel, useDisabled } from "../../app/disabled";
import { UiStore, type Route } from "../../app/ui";
import { Toasts } from "../../design/Toast";
import { createSimPlatform } from "../../dev/sim/platform";
import { ids } from "../../dev/sim/scenario";
import { CommandError, OutcomeUnknownError, HostSession } from "../../host/session";
import { RouteProvider } from "../../shell/Screen";
import type { Query, QueryData } from "../../protocol/types";
import Library from "./Library";
import AgentFiles from "./AgentFiles";
import File from "./File";
import { DisableControl, shownDisabled } from "./Disabled";
import { StartFromSheet } from "../launch/StartFrom";

const SKILL = "/home/sam/.claude/skills/design-review/SKILL.md";
const AGENT = "/home/sam/.claude/agents/reviewer.md";
const FILE = "/home/sam/Projects/tidewater-api/CLAUDE.md";

let app: App;
const tick = async (ms = 900) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(async () => {
  localStorage.clear(); vi.useFakeTimers(); resetDisabled();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.stubGlobal("crypto", { getRandomValues: crypto.getRandomValues.bind(crypto), randomUUID: crypto.randomUUID.bind(crypto) });
  const platform = createSimPlatform();
  const profile = (await platform.profiles())[0]!;
  const session = new HostSession(profile.profileId, platform.control(profile.profileId));
  app = { platform, profile, session, profiles: [profile], ui: new UiStore(profile.profileId), switchTo() {}, pairAnother() {}, async rename() {}, async forget() {} };
  session.start(); await tick();
  app.ui.selectProject(ids.pHome);
});
afterEach(() => { cleanup(); app.session.stop(); window.__sim?.host.dispose(); vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

const wrap = (route: Route, node: React.ReactNode) =>
  render(<AppProvider app={app}><RouteProvider value={{ tab: "library", depth: 1, active: true, route }}>{node}<Toasts /></RouteProvider></AppProvider>);
const library = () => wrap({ screen: "library" }, <Library route={{ screen: "library" }} />);
const disabledSection = () => screen.queryByRole("region", { name: "Disabled in Øphio" });
const sent = () => Object.values(app.session.commands.get()).filter((c) => c.body.command === "set_disabled_in_ophio").map((c) => c.body);
const skills = () => fireEvent.click(screen.getByRole("radio", { name: /^Skills/ }));

it("disables a skill from its sheet at once, holds it apart below the live ones, and enables it again", async () => {
  library(); await tick(); skills();
  fireEvent.click(screen.getByRole("button", { name: "About design-review" })); await tick();
  const sheet = screen.getByRole("dialog", { name: /design-review/ });
  expect(within(sheet).getByText(/won’t see it, list it or use it\. The file isn’t changed/)).toBeTruthy();
  fireEvent.click(within(sheet).getByRole("button", { name: "Disable in Øphio" }));
  // Shown before the computer has answered.
  expect(within(sheet).getByRole("button", { name: "Enable in Øphio" })).toBeTruthy();
  expect(within(sheet).queryByRole("button", { name: "Start from this" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Start from design-review" })).toBeNull();
  const held = within(disabledSection()!);
  expect(held.getByText("design-review")).toBeTruthy();
  expect(held.getByText(/^Skill · /)).toBeTruthy();
  expect(sent()).toEqual([{ command: "set_disabled_in_ophio", kind: "skill", host_path: SKILL, disabled: true }]);
  await tick();
  expect(window.__sim!.host.disabled.has(SKILL)).toBe(true);
  // From the Disabled section it comes straight back.
  fireEvent.click(held.getByRole("button", { name: "Enable design-review in Øphio" }));
  expect(disabledSection()).toBeNull();
  expect(screen.getByRole("button", { name: "Start from design-review" })).toBeTruthy();
  await tick();
  expect(window.__sim!.host.disabled.has(SKILL)).toBe(false);
});

it("puts a refused change back and says why", async () => {
  library(); await tick(); skills();
  vi.spyOn(app.session, "command").mockRejectedValueOnce(new CommandError({ code: "permission_denied", message: "This device can’t change Library." }, null));
  fireEvent.click(screen.getByRole("button", { name: "About design-review" })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Disable in Øphio" }));
  expect(disabledSection()).not.toBeNull();
  await tick();
  expect(disabledSection()).toBeNull();
  expect(screen.getByRole("button", { name: "Disable in Øphio" })).toBeTruthy();
  expect(screen.getByText(/design-review is still enabled\./)).toBeTruthy();
});

it("says why one of Øphio's own skills can't be disabled", async () => {
  library(); await tick(); skills();
  fireEvent.click(screen.getByRole("button", { name: "About setup-phone" })); await tick();
  const sheet = screen.getByRole("dialog", { name: /setup-phone/ });
  expect(within(sheet).getByText("Can’t be disabled in Øphio.")).toBeTruthy();
  expect(within(sheet).getByText(/Built into Øphio, whose own setup and GitHub work start from it\./)).toBeTruthy();
  expect(within(sheet).queryByRole("button", { name: "Disable in Øphio" })).toBeNull();
});

it("holds disabled skills, agents and files together, each with its kind, below every segment", async () => {
  await act(async () => { const seeded = window.__sim!.host.disabled.seed([SKILL, AGENT, FILE]); await vi.advanceTimersByTimeAsync(900); await seeded; });
  library(); await tick();
  const kinds = () => within(disabledSection()!).getAllByText(/^(Skill|Agent|Instructions) · /).map((n) => n.textContent!.split(" · ")[0]).sort();
  expect(kinds()).toEqual(["Agent", "Instructions", "Skill"]);
  // The disabled agent isn't among the live ones above it.
  expect(screen.queryByRole("button", { name: /^Start from reviewer/ })).toBeNull();
  skills();
  expect(kinds()).toEqual(["Agent", "Instructions", "Skill"]);
  fireEvent.click(screen.getByRole("radio", { name: "Files" })); await tick();
  expect(kinds()).toEqual(["Agent", "Instructions", "Skill"]);
});

it("keeps a disabled file out of its kind's list and lets its own screen enable it", async () => {
  await act(async () => { const seeded = window.__sim!.host.disabled.seed([FILE]); await vi.advanceTimersByTimeAsync(900); await seeded; });
  const route = { screen: "agent_files", group: "instructions" } as const;
  const view = wrap(route, <AgentFiles route={route} />); await tick();
  const project = screen.getByRole("heading", { level: 2, name: "tidewater-api" }).closest("section")!;
  expect(within(project).queryByText("CLAUDE.md")).toBeNull();
  expect(within(disabledSection()!).getByText("CLAUDE.md")).toBeTruthy();
  view.unmount();
  const file = { screen: "file", rootId: ids.rootInstr, path: "CLAUDE.md" } as const;
  wrap(file, <File route={file} />); await tick(); await tick();
  expect(screen.getByText("Disabled in Øphio", { selector: ".notice__title" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Enable in Øphio" }));
  expect(screen.queryByText("Disabled in Øphio", { selector: ".notice__title" })).toBeNull();
  expect(screen.getByRole("button", { name: "Disable in Øphio" })).toBeTruthy();
  await tick();
  expect(window.__sim!.host.disabled.has(FILE)).toBe(false);
});

it("offers nothing on a computer that can't keep things from its agents", async () => {
  const host = window.__sim!.host;
  host.host = { ...host.host, capabilities: { ...host.host.capabilities, features: host.host.capabilities.features.filter((f) => f !== "disable_in_ophio_v1") } };
  app.session.stop(); app.session.start(); await tick();
  library(); await tick(); skills();
  fireEvent.click(screen.getByRole("button", { name: "About design-review" })); await tick();
  expect(screen.queryByRole("button", { name: "Disable in Øphio" })).toBeNull();
  expect(screen.queryByText(/Can’t be disabled/)).toBeNull();
  expect(disabledSection()).toBeNull();
});

/** Holds the catalog's answer until `release`, or fails it. */
function holdCatalog(answer?: (q: Query) => QueryData | Error) {
  const real = app.session.query.bind(app.session);
  let release!: () => void;
  const held = new Promise<void>((r) => { release = r; });
  vi.spyOn(app.session, "query").mockImplementation(async (q: Query) => {
    if (q.kind !== "catalog") return real(q);
    await held;
    const out = answer?.(q);
    if (out instanceof Error) throw out;
    return out ?? real(q);
  });
  return () => release();
}

it("says it is checking until the first read ends, never an empty list", async () => {
  const release = holdCatalog();
  library(); await tick(3000);
  const checking = screen.getByText("Checking for agents, skills and files…").closest("[role=status]")!;
  expect(checking.getAttribute("aria-live")).toBe("polite");
  expect(checking.getAttribute("aria-busy")).toBe("true");
  expect(screen.queryByText(/Agents are Markdown files/)).toBeNull();
  release(); await tick();
  expect(screen.queryByText("Checking for agents, skills and files…")).toBeNull();
  expect(screen.getByRole("button", { name: /^Scratch pad/ })).toBeTruthy();
});

it("says a list is empty only once it is read", async () => {
  const release = holdCatalog(() => ({ kind: "catalog", catalog: { entries: [], one_offs: null, agents_folder: null, problems: [], truncated: false } }));
  library(); await tick();
  skills();
  expect(screen.queryByText(/Skills are folders with a SKILL\.md/)).toBeNull();
  release(); await tick();
  expect(screen.getByText(/Skills are folders with a SKILL\.md/)).toBeTruthy();
});

it("shows a failed read, not an endless spinner", async () => {
  const release = holdCatalog(() => new Error("The computer didn’t answer."));
  library(); await tick();
  release(); await tick();
  expect(screen.getByText("Couldn’t read the list")).toBeTruthy();
  expect(screen.queryByText("Checking for agents, skills and files…")).toBeNull();
});

it("keeps the list while it checks again, with a small line", async () => {
  const route = { screen: "library" } as const;
  const node = (active: boolean) => (
    <AppProvider app={app}><RouteProvider value={{ tab: "library", depth: 1, active, route }}><Library route={route} /></RouteProvider></AppProvider>
  );
  const view = render(node(true)); await tick();
  const release = holdCatalog();
  // Back from another screen, Library reads the list again.
  view.rerender(node(false)); view.rerender(node(true)); await tick();
  expect(screen.getByText("Checking again…")).toBeTruthy();
  expect(screen.getByRole("button", { name: /^Scratch pad/ })).toBeTruthy();
  release(); await tick();
  expect(screen.queryByText("Checking again…")).toBeNull();
});

const COMMAND = "/home/sam/.claude/commands/changelog.md";

it("shows a disabled command under Commands and calls it a Command", async () => {
  await act(async () => { const seeded = window.__sim!.host.disabled.seed([COMMAND]); await vi.advanceTimersByTimeAsync(900); await seeded; });
  const route = { screen: "agent_files", group: "commands" } as const;
  wrap(route, <AgentFiles route={route} />); await tick();
  expect(within(disabledSection()!).getByText("changelog.md")).toBeTruthy();
  expect(within(disabledSection()!).getByText(/^Command · /)).toBeTruthy();
  expect(screen.queryAllByRole("button", { name: /changelog.md/ }).filter((button) => !disabledSection()!.contains(button))).toEqual([]);
  expect(shownDisabled(window.__sim!.host.disabled.list(), null, "skills")).toEqual([]);
  expect(kindLabel({ kind: "skill", group: "commands" })).toBe("Command");
});

it("does not hide cached disabled rows on a computer without the feature", async () => {
  await act(async () => { const seeded = window.__sim!.host.disabled.seed([SKILL]); await vi.advanceTimersByTimeAsync(900); await seeded; });
  library(); await tick(); skills();
  expect(screen.queryByRole("button", { name: "Start from design-review" })).toBeNull();
  const host = window.__sim!.host;
  host.host = { ...host.host, capabilities: { ...host.host.capabilities, features: host.host.capabilities.features.filter((f) => f !== "disable_in_ophio_v1") } };
  app.session.stop(); app.session.start(); await tick();
  expect(disabledSection()).toBeNull();
  expect(screen.getByRole("button", { name: "Start from design-review" })).toBeTruthy();
});

it("uses the shared optimistic state in Chat's Start from choices", async () => {
  const route = { screen: "library" } as const;
  const view = wrap(route, <Library route={route} />); await tick(); skills();
  vi.spyOn(app.session, "command").mockImplementation(() => new Promise(() => {}));
  fireEvent.click(screen.getByRole("button", { name: "About design-review" })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Disable in Øphio" }));
  view.rerender(<AppProvider app={app}><RouteProvider value={{ tab: "library", depth: 1, active: true, route }}><StartFromSheet open onClose={() => {}} projectId={null} onChoose={() => {}} only="skill" /></RouteProvider></AppProvider>);
  await tick();
  expect(screen.queryByRole("button", { name: /design-reviewReview a UI/ })).toBeNull();
});

it("uses the computer's key but sends its listed link path and retains the flip after settling", async () => {
  const link = "/home/sam/.claude/skills/link/SKILL.md", key = SKILL;
  const real = app.session.query.bind(app.session);
  vi.spyOn(app.session, "query").mockImplementation(async (q: Query) => q.kind === "disabled_in_ophio"
    ? { kind: "disabled_in_ophio", items: window.__sim!.host.disabled.list() } as Extract<QueryData, { kind: "disabled_in_ophio" }>
    : real(q));
  const Control = () => { const d = useDisabled(); return <><span>{d.isDisabled(key, false) ? "Held" : "Live"}</span><DisableControl disable={{ state: "can", kind: "skill", disabled: false, key }} target={{ kind: "skill", host_path: link, key, name: "Linked skill", scope: "Claude Code" }} /></>; };
  const command = vi.spyOn(app.session, "command").mockImplementation(async () => {
    window.__sim!.host.disabled.items.set(key, { kind: "skill", host_path: key, name: "Linked skill", scope: "Claude Code", description: null, harness: null, project_id: null, group: null, library: null, disabled_at: Date.now(), found: true });
    return {} as Awaited<ReturnType<HostSession["command"]>>;
  });
  wrap({ screen: "library" }, <Control />); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Disable in Øphio" }));
  expect(screen.getByText("Held")).toBeTruthy();
  expect(command.mock.calls[0]?.[0]).toEqual({ command: "set_disabled_in_ophio", kind: "skill", host_path: link, disabled: true });
  await tick(6000);
  expect(screen.getByText("Held")).toBeTruthy();
});

it("does not claim an unknown command was refused and checks the next read", async () => {
  library(); await tick(); skills();
  vi.spyOn(app.session, "command").mockRejectedValueOnce(new OutcomeUnknownError("unknown"));
  fireEvent.click(screen.getByRole("button", { name: "About design-review" })); await tick();
  fireEvent.click(screen.getByRole("button", { name: "Disable in Øphio" }));
  await tick(100);
  expect(screen.getByText(/Not sure .* got that/)).toBeTruthy();
  await tick(6000);
  expect(screen.getByRole("button", { name: "Disable in Øphio" })).toBeTruthy();
});

it.each([
  { listedAfter: true, expected: "Held" },
  { listedAfter: false, expected: "Live" },
])("keeps an unknown disable through a stale read, then settles a fresh read (listed=$listedAfter)", async ({ listedAfter, expected }) => {
  const real = app.session.query.bind(app.session);
  let release!: (items: Extract<QueryData, { kind: "disabled_in_ophio" }>["items"]) => void;
  const held = new Promise<Extract<QueryData, { kind: "disabled_in_ophio" }>["items"]>((resolve) => { release = resolve; });
  let releaseFresh!: (items: Extract<QueryData, { kind: "disabled_in_ophio" }>["items"]) => void;
  const fresh = new Promise<Extract<QueryData, { kind: "disabled_in_ophio" }>["items"]>((resolve) => { releaseFresh = resolve; });
  let reads = 0;
  const query = vi.spyOn(app.session, "query").mockImplementation((q: Query) => {
    if (q.kind !== "disabled_in_ophio") return real(q);
    reads++;
    if (reads === 2) return held.then((items) => ({ kind: "disabled_in_ophio", items }));
    if (reads === 3) return fresh.then((items) => ({ kind: "disabled_in_ophio", items }));
    return real(q);
  });
  const Control = () => {
    const d = useDisabled();
    return <><span>{d.isDisabled(SKILL, false) ? "Held" : "Live"}</span>
      <button onClick={d.refresh}>Read again</button>
      <button onClick={() => d.set({ kind: "skill", host_path: SKILL, name: "design-review", scope: "Claude Code" }, true)}>Disable</button></>;
  };
  wrap({ screen: "library" }, <Control />);
  await tick();
  fireEvent.click(screen.getByRole("button", { name: "Read again" }));
  expect(reads).toBe(2);
  vi.spyOn(app.session, "command").mockRejectedValueOnce(new OutcomeUnknownError("unknown"));
  fireEvent.click(screen.getByRole("button", { name: "Disable" }));
  await act(async () => { await Promise.resolve(); });
  expect(screen.getByText("Held")).toBeTruthy();
  await act(async () => { release([]); await held; });
  expect(screen.getByText("Held")).toBeTruthy();
  await act(async () => { await Promise.resolve(); await Promise.resolve(); });
  expect(query.mock.calls.filter(([q]) => q.kind === "disabled_in_ophio")).toHaveLength(3);
  expect(screen.getByText("Held")).toBeTruthy();
  await act(async () => { releaseFresh(listedAfter ? [{
    kind: "skill", host_path: SKILL, name: "design-review", description: null,
    scope: "Claude Code", harness: null, project_id: null, group: null,
    library: null, disabled_at: Date.now(), found: true,
  }] : []); await fresh; });
  expect(screen.getByText(expected)).toBeTruthy();
});

it("does not scan agent files when an ordinary document opens", async () => {
  const real = app.session.query.bind(app.session);
  const query = vi.spyOn(app.session, "query").mockImplementation((q: Query) => real(q));
  const route = { screen: "file", rootId: ids.rootInstr, path: "README.md" } as const;
  wrap(route, <File route={route} />); await tick();
  expect(query.mock.calls.filter(([q]) => q.kind === "agent_files")).toHaveLength(0);
});
