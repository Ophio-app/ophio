// SPDX-License-Identifier: Apache-2.0
//
// Phone-local interface state: where the person is, what they were looking
// at and what they have typed but not sent. Persisted per computer so a
// restart returns to the same place. Nothing here is host truth.

import type { AgentFileGroup, Command, ConversationAccess, HarnessKind, MessageDelivery, QueueAuthorization, TaskBranch, TaskMode } from "../protocol/types";
import { followRoutes } from "../diagnostics/instrumentation";
import { uuidv7 } from "../protocol/ids";
import { Store } from "../state/store";
import { preserveSavedText, readSavedJson, UnpreservedJson } from "../state/savedJson";
import type { LaunchStart } from "./launchers";
import type { TalkShows } from "./taskOnScreen";

export type Tab = "talk" | "control" | "work" | "library" | "todo";

export const TABS: readonly Tab[] = ["talk", "control", "work", "library", "todo"];

const TAB_LABELS: Record<Tab, string> = { talk: "Chat", control: "Control", work: "Tasks", library: "Library", todo: "To-Do" };
/** A tab by the name its button shows. Kept here, not with the bottom nav, so a
 * screen naming a tab doesn't share the nav's chunk and load its styles early. */
export const tabLabel = (t: Tab): string => TAB_LABELS[t];

/** Markup drawn while a run waits for input back on its target: sending it
 * also gives that input back, so the agent resumes with the drawing. */
export type HandBackRoute = { targetId: string; runId: string };

/** One destination inside a tab. Screen names are logical, not URLs. */
export type Route =
  | { screen: "talk" }
  /** Review what will be sent before Start/Send. */
  | { screen: "review"; projectId: string; taskId: string | null; intent?: ReviewIntent; queueItemId?: string; plan?: { text: string; runId: string; number: number; latest: boolean }; handBack?: HandBackRoute }
  /** Full-screen annotation of a phone-local draft. `intent` is how
   * the screen it was opened from was set to send it, which its Review keeps. */
  | { screen: "annotate"; annotationId: string; intent?: ReviewIntent; handBack?: HandBackRoute }
  | { screen: "control" }
  | { screen: "desktop"; targetId: string }
  | { screen: "computer_use"; taskId: string }
  | { screen: "terminal"; terminalId: string }
  /** The computer's load, a door from Control's Screens. */
  | { screen: "system" }
  | { screen: "work" }
  | { screen: "agents" }
  | { screen: "receipt"; taskId: string; runId?: string; section?: "changes" }
  | { screen: "next_up" }
  | { screen: "inbox" }
  | { screen: "attention"; attentionId: string }
  | { screen: "needs_you"; projectId: string }
  | { screen: "library" }
  | { screen: "folder"; rootId: string; path: string }
  /** Library's agent files of one kind, wherever they are kept. */
  | { screen: "agent_files"; group: AgentFileGroup }
  | { screen: "file"; rootId: string; path: string; taskId?: string; source?: "instructions" | "receipt"; /** Open straight into the editor, for a file just made to be edited. */ edit?: boolean }
  | { screen: "conflict"; rootId: string; path: string }
  | { screen: "transfers" }
  | { screen: "connections" }
  | { screen: "recently_deleted" }
  | { screen: "instructions"; taskId: string }
  | { screen: "reusable"; taskId: string }
  | { screen: "computer_use_review"; projectId: string }
  | { screen: "settings"; section?: string; projectId?: string; reportId?: string }
  | { screen: "accounts" }
  | { screen: "agent_logins" }
  | { screen: "voice"; sessionId?: string }
  | { screen: "windows"; targetId: string }
  | { screen: "library_recovery"; rootId: string; recoveryId: string }
  | { screen: "stages"; projectId: string }
  /** A project's GitHub issues, or its Find & fix: reviews, their fixes and sorting. Without a project, the list of Git projects. */
  | { screen: "github"; projectId?: string; view?: "issues" | "find_fix" }
  /** Filing a new issue, or starting a new review, in a project; review_only files what it finds and fixes nothing. */
  | { screen: "github_new"; projectId: string; kind: "issue" | "review" | "review_only" }
  /** A project's open pull requests. */
  | { screen: "github_list"; projectId: string }
  /** A handoff from Clankerhub: where it goes, then a paused project. */
  | { screen: "contribution"; handoffId: string }
  /** That handoff's rules and the person's own check that it's Clankerhub's. */
  | { screen: "contribution_rules"; handoffId: string }
  /** Find & fix without GitHub: the project's FINDINGS.md. */
  | { screen: "findings"; projectId: string }
  /** Starting a review that writes FINDINGS.md, and with fix, a fixer queued after it. */
  | { screen: "findings_new"; projectId: string; fix: boolean }
  /** What the person has to do themselves in a project, with `itemId` open. */
  | { screen: "checklist"; projectId: string; itemId?: string }
  | { screen: "todo" };

/** Home tab for an outside destination. In-app navigation keeps its existing stack. */
export function homeTab(route: Route): Tab {
  switch (route.screen) {
    case "talk": case "review": case "annotate": case "voice": return "talk";
    case "control": case "desktop": case "computer_use": case "terminal": case "system": case "computer_use_review": case "windows": return "control";
    case "library": case "folder": case "agent_files": case "file": case "conflict": case "recently_deleted": case "instructions": case "reusable": case "library_recovery": case "transfers": return "library";
    case "todo": case "checklist": return "todo";
    case "work": case "agents": case "receipt": case "next_up": case "inbox": case "attention": case "needs_you": case "connections": case "settings": case "accounts": case "agent_logins": case "stages": case "github": case "github_list": case "github_new": case "findings": case "findings_new": case "contribution": case "contribution_rules": return "work";
  }
}

/** What a Review leaves behind once it is done: itself and the markup it carried. */
export const REVIEW_FLOW: Route["screen"][] = ["review", "annotate"];

/** What Request review is confirming; a delivery means a message to the task. */
export type ReviewIntent = "carry_out_plan" | "new_task" | "next_up" | "continue" | MessageDelivery;

export const ROOT: Record<Tab, Route> = {
  talk: { screen: "talk" },
  control: { screen: "control" },
  work: { screen: "work" },
  library: { screen: "library" },
  todo: { screen: "todo" },
};

/** Every screen this version has. A saved stack can hold one it doesn't: a
 * newer version's, after rolling back, or one since retired. */
const SCREENS: Record<Route["screen"], true> = {
  talk: true, review: true, annotate: true, control: true, desktop: true, computer_use: true, terminal: true,
  system: true, work: true, agents: true, receipt: true, next_up: true, inbox: true, attention: true,
  needs_you: true, library: true, folder: true, agent_files: true, file: true, conflict: true, transfers: true,
  connections: true, recently_deleted: true, instructions: true, reusable: true, computer_use_review: true,
  settings: true, accounts: true, agent_logins: true, voice: true, windows: true, library_recovery: true,
  stages: true, github: true, github_new: true, github_list: true, contribution: true, contribution_rules: true,
  findings: true, findings_new: true, checklist: true, todo: true,
};

/** Text the person typed and has not sent, bound to where it was typed. */
export interface Draft {
  projectId: string;
  /** null = a new task in this project. */
  taskId: string | null;
  text: string;
  /** Phone-local annotation draft attached to this message (state/annotations). */
  annotationId: string | null;
  /** Agent chosen for a new task when it differs from the project default. */
  agent?: AgentChoice | null;
  /** Requested mode for a new task; absent means Work. */
  mode?: TaskMode;
  /** What a new conversation may reach; absent is its own project, read-only. */
  access?: ConversationAccess;
  /** The last send whose outcome was not a clean acceptance; kept until resolved. */
  attempt?: SendAttempt | null;
  /** Choices made in Review for a new task, kept until it is sent. */
  review?: ReviewDetails | null;
  updatedAt: number;
}

export interface ReviewDetails {
  title: string;
  stopWhen: string;
  /** null = the default for how Review was opened. */
  placement: "start_now" | "queue" | null;
  authorization: QueueAuthorization;
  bounds?: string;
  completionChecks?: string;
  /** The Git branch to tell the agent. Undefined is the default for how
   * Review was opened; null is All branches, naming none. */
  branch?: TaskBranch | null;
  /** The agent, skill or scratch pad it starts from; absent is an ordinary task. */
  start?: LaunchStart | null;
  /** Where it works. Absent is the project the draft belongs to; a
   * draft with no project to belong to is always on its own. */
  place?: "project" | "one_off";
  /** Work runs may use the agent browser and logins; absent is yes. */
  browser?: boolean;
  /** What GitHub work it is for. Undefined is the default for the project's
   * GitHub setting; null is not linked. */
  github?: import("../protocol/types").GithubLink | null;
  /** The mode of the task an agent proposed this from, so Review says when
   * starting it switches. Absent when nothing proposed it. */
  from?: import("../protocol/types").TaskMode;
}

/** Review's choices before any are made. Queued work starts by itself unless the person asks to be asked first. */
export const freshReview = (): ReviewDetails => ({ title: "", stopWhen: "", placement: null, authorization: "auto_start" });

export const hasReviewDetails = (r: ReviewDetails | null | undefined): boolean =>
  !!r && (r.title.trim() !== "" || r.stopWhen.trim() !== "" || !!r.bounds?.trim() || !!r.completionChecks?.trim() || r.placement !== null || r.authorization !== "auto_start" || r.branch != null || !!r.start || r.place === "one_off" || r.browser === false || r.github != null);

/** What New issue does once the issue is filed. */
export type IssueThen = "start" | "next_up" | "file";

/** An issue being written in New issue, kept until it is filed and done
 * with, or discarded, so a failed refresh or Back doesn't lose it. */
export interface IssueDraft {
  /** Which draft, so an answer for one never lands on another. */
  id: string;
  title: string;
  body: string;
  then: IssueThen;
  /** Chosen for its work instead of the agent last remembered here. */
  agent: AgentChoice | null;
  /** Another agent reviews its code, and the one chosen for that, if any. */
  reviewOn: boolean;
  reviewer: AgentChoice | null;
  /** Filed on GitHub already, so trying again only starts the work. */
  filed: { number: number; url: string } | null;
  /** Filing it or starting its work, on its way. Unknown once the app
   * closed before it heard how that went. */
  pending: { what: "filing" | "starting"; unknown: boolean } | null;
  updatedAt: number;
}

export interface AgentChoice {
  harness: HarnessKind;
  model: string | null;
  effort: string | null;
  /** null preserves an older task's native behavior; undefined uses phone defaults. */
  launchPolicy?: import("../protocol/types").LaunchPolicy | null;
}

export interface SendAttempt {
  /** Deduplication history at send time; missing/changed means a repeat is unsafe. */
  historyId?: string;

  /** `sending` is saved before the command leaves the phone, so a restart
   * part way through still knows which command to ask about. */
  phase: "sending" | "unknown" | "rejected" | "not_delivered" | "failed";
  at: number;
  /** Set when the command may have reached the computer, so its fate can be asked later. */
  commandId: string | null;
  /** How a message to the task's current run was to arrive; absent for any
   * other request. Once that run is over, sending it would start a new one. */
  delivery?: MessageDelivery;
  detail: string;
  /** What was sent, so a later confirmation clears only that text. */
  text: string;
  /** A one-turn question that changes nothing, such as Summarize. Its text
   * isn't the draft's, so trying again asks it again rather than sending the field. */
  readOnly?: boolean;
  /** A conversation carried on as a task. Its text is the task's words, so
   * trying again opens Turn into a task with them rather than replying. */
  asTask?: boolean;
  /** The markup sent with it, cleared from the draft only if unchanged since. */
  annotationId?: string | null;
  /** The exact request, so sending again reuses its id and the computer
   * recognises a repeat instead of acting twice. */
  body?: Command | null;
  expectedRevision?: number | null;
  /** The markup exactly as submitted, so a confirmation learned after a
   * restart marks only that content as sent, never later edits. */
  markup?: { revision: number; content: string; labels?: number } | null;
}

/** Markup taken off a message and not yet deleted: the message it came off
 * and its content then. */
export interface RemovedMarkup {
  projectId: string;
  taskId: string | null;
  removedAs: string | null;
}

/** Where Back leaves a tab for: the tab, and in Talk the task, a screen
 * or task was opened from. */
export interface Return {
  tab: Tab;
  /** The tab's stack length at which Back leaves rather than steps back. */
  depth: number;
  /** Opened from another task in Talk: the task to show again. */
  task?: { projectId: string; taskId: string | null };
}

export interface UiState {
  tab: Tab;
  stacks: Record<Tab, Route[]>;
  /** Per tab, where Back goes once it is back at what was opened there. */
  returns: Partial<Record<Tab, Return>>;
  projectId: string | null;
  /** To-Do can combine projects without changing the selected project elsewhere. */
  todoAll: boolean;
  /** Item opened by a checklist link; null means the normal To-Do view. */
  todoItemId: string | null;
  /** Task shown in Talk per project; null = compose a new task, or a new
   * conversation while `conversing`. */
  taskByProject: Record<string, string | null>;
  /** Talk shows the project's conversations instead of a task, as a
   * messaging app's chats, with the tabs up. Choosing one leaves it. */
  taskList: boolean;
  /** With no task chosen, Talk starts a conversation rather than a task. */
  conversing: boolean;
  drafts: Record<string, Draft>;
  savedDrafts: Record<string, Draft[]>;
  /** Per project, the issue being written in New issue. */
  issueDrafts: Record<string, IssueDraft>;
  targetId: string | null;
  /** Kept with the interface, so that after a restart removed markup still
   * becomes the message's again when changed, or is deleted once let go. */
  removedMarkup: Record<string, RemovedMarkup>;
}

export const draftKey = (projectId: string, taskId: string | null) => `${projectId}/${taskId ?? "new"}`;

const initial: UiState = {
  tab: "talk",
  stacks: { talk: [ROOT.talk], control: [ROOT.control], work: [ROOT.work], library: [ROOT.library], todo: [ROOT.todo] },
  returns: {},
  projectId: null,
  todoAll: false,
  todoItemId: null,
  taskByProject: {},
  taskList: false,
  conversing: false,
  drafts: {},
  savedDrafts: {},
  issueDrafts: {},
  targetId: null,
  removedMarkup: {},
};

const VERSION = 2;
const STACK_LIMIT = 12;
/** Keep each tab’s home beneath its most recent destinations. */
function rooted(tab: Tab, stack: Route[]): Route[] {
  const above = stack[0]?.screen === ROOT[tab].screen ? stack.slice(1) : stack;
  return [ROOT[tab], ...above.slice(-(STACK_LIMIT - 1))];
}

const RECOVERED = "Saved chats were damaged. A copy has been kept on this device for recovery. New changes can be saved again.";
const readError = (error: unknown) => error instanceof UnpreservedJson
  ? "Saved chats are damaged and could not be set aside. Keep the app open; new drafts are only held in memory."
  : "Saved chats could not be read. Keep the app open; new drafts are only held in memory until storage recovers.";

function load(key: string): { state: UiState; recovered: boolean } {
  const { value, recovered } = readSavedJson(localStorage, key);
  const v = value as { version: number; state: UiState } | null;
  if (!v || v.version !== VERSION) return { state: initial, recovered };
  const s = { ...initial, ...v.state };
  // A checklist link could have been opened from any tab before To-Do existed.
  // Preserve the underlying destinations and drafts, but put an active legacy
  // link on To-Do with its project and item selected.
  const active = TABS.includes(s.tab) && Array.isArray(s.stacks?.[s.tab]) ? s.stacks[s.tab].at(-1) : undefined;
  if (active?.screen === "checklist") {
    s.tab = "todo";
    s.projectId = active.projectId;
    s.todoItemId = active.itemId ?? null;
    s.todoAll = false;
  }
  // Agents had a tab of its own until Library took it in.
  if (!TABS.includes(s.tab)) s.tab = "library";
  // All tasks had a screen of its own until Work's Open, Finished and
  // Archived took it in. Neither old destination belongs on another tab, and
  // nor does a screen this version doesn't have.
  const kept = (r: Route) => typeof r?.screen === "string" && r.screen !== "checklist" && Object.hasOwn(SCREENS, r.screen);
  const removed = {} as Record<Tab, Route[]>;
  s.stacks = Object.fromEntries(TABS.map((tab) => {
    const stack = Array.isArray(s.stacks?.[tab]) ? s.stacks[tab] : [];
    removed[tab] = stack;
    return [tab, rooted(tab, stack.filter(kept))];
  })) as UiState["stacks"];
  s.returns = Object.fromEntries(Object.entries(s.returns ?? {}).flatMap(([tab, r]) => {
    if (!TABS.includes(tab as Tab) || !TABS.includes(r.tab)) return [];
    const stack = removed[tab as Tab];
    const opened = stack[r.depth - 1];
    if (opened && !kept(opened)) return [];
    const depth = r.depth - stack.slice(0, r.depth).filter((route) => !kept(route)).length;
    return depth >= 1 && depth <= s.stacks[tab as Tab].length ? [[tab, { ...r, depth }]] : [];
  })) as UiState["returns"];
  if (active?.screen === "checklist") s.returns = without(s.returns, "todo");
  // A send still in progress when the app last closed has an unknown fate.
  for (const [key, d] of Object.entries(s.issueDrafts ?? {}))
    if (d.pending && !d.pending.unknown) s.issueDrafts = { ...s.issueDrafts, [key]: { ...d, pending: { ...d.pending, unknown: true } } };
  for (const [key, d] of Object.entries(s.drafts))
    if (d.attempt?.phase === "sending")
      s.drafts = {
        ...s.drafts,
        [key]: {
          ...d,
          attempt: { ...d.attempt, phase: "unknown", detail: "The app closed before the computer answered. It may have arrived." },
        },
      };
  return { state: s, recovered };
}

/** Work made while storage was unreadable must not replace an earlier draft
 * for the same conversation. Both remain selectable in Chat's Saved drafts. */
function mergeRecovered(saved: UiState, current: UiState): UiState {
  const savedDrafts = { ...saved.savedDrafts };
  for (const [key, drafts] of Object.entries(current.savedDrafts))
    savedDrafts[key] = [...(savedDrafts[key] ?? []), ...drafts];
  for (const [key, draft] of Object.entries(saved.drafts))
    if (current.drafts[key]) savedDrafts[key] = [...(savedDrafts[key] ?? []), draft];
  return {
    ...saved, ...current,
    drafts: { ...saved.drafts, ...current.drafts },
    issueDrafts: { ...saved.issueDrafts, ...current.issueDrafts },
    savedDrafts,
    removedMarkup: { ...saved.removedMarkup, ...current.removedMarkup },
    taskByProject: { ...saved.taskByProject, ...current.taskByProject },
  };
}

const without = (returns: UiState["returns"], tab: Tab): UiState["returns"] => {
  if (!returns[tab]) return returns;
  const next = { ...returns };
  delete next[tab];
  return next;
};

/** Back to where the current tab's screen or task was opened from, once
 * Back has reached it; null while there are steps back within the tab. */
function leave(s: UiState): UiState | null {
  const r = s.returns[s.tab];
  const stack = s.stacks[s.tab];
  if (!r || stack.length !== r.depth || (s.tab === "talk" && stack.length === 1 && s.taskList)) return null;
  const stacks = stack.length > 1 ? { ...s.stacks, [s.tab]: stack.slice(0, -1) } : s.stacks;
  const returns = without(s.returns, s.tab);
  // A task opened from elsewhere leaves Talk on its conversations, not on it.
  if (r.tab !== s.tab) return { ...s, tab: r.tab, stacks, returns, taskList: s.tab === "talk" || s.taskList };
  if (!r.task) return null;
  // Another task in Talk: show it again, as it was.
  const { projectId, taskId } = r.task;
  return { ...s, stacks: { ...stacks, talk: [ROOT.talk] }, returns, projectId, taskByProject: { ...s.taskByProject, [projectId]: taskId }, taskList: false, conversing: false };
}

const listTasks = (s: UiState): UiState =>
  ({ ...s, tab: "talk", taskList: true, stacks: { ...s.stacks, talk: [ROOT.talk] }, returns: without(s.returns, "talk") });

/** Talk showing `taskId`, its stack kept only while it still belongs to it. */
function choose(s: UiState, projectId: string, taskId: string | null): UiState {
  const changed = s.projectId !== projectId || s.taskByProject[projectId] !== taskId;
  const mismatched = s.stacks.talk.some((r) =>
    ("taskId" in r && r.taskId !== taskId) || ("projectId" in r && r.projectId !== projectId));
  return { ...s, projectId, taskByProject: { ...s.taskByProject, [projectId]: taskId }, taskList: false, conversing: false,
    stacks: changed || mismatched ? { ...s.stacks, talk: [ROOT.talk] } : s.stacks };
}

/** How long a takeover asked for elsewhere may wait for Desktop to be ready. */
export const TAKEOVER_MS = 10_000;

/** Where a drawing starts: choosing a screen to capture, or images or a
 * clip the person already picked on this device. */
export type SourceStart = { kind: "screen" } | { kind: "files"; files: File[] };

/** How long Talk waits for a task this device created to reach its record. */
export const ARRIVAL_MS = 10_000;

export class UiStore extends Store<UiState> {
  #key: string;
  #unread = false;
  #flushing = false;
  readonly persistence = new Store<string | null>(null);
  readonly recovery = new Store<string | null>(null);
  /** Nothing was kept from an earlier run: this device's first look at this computer. */
  #firstRun: boolean;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #created = new Map<string, number>();
  #forgotten = false;
  #hide = () => this.#flush();

  constructor(profileId: string) {
    const key = `ui.${profileId}`;
    let state = initial, error: string | null = null, recovered = false;
    try { ({ state, recovered } = load(key)); }
    catch (e) { error = readError(e); }
    super(state);
    this.#key = key;
    this.#unread = error !== null;
    this.persistence.set(error);
    if (recovered) this.recovery.set(RECOVERED);
    this.#firstRun = state === initial && !this.#unread && !recovered;
    this.subscribe(() => this.#persist());
    followRoutes(this);
    window.addEventListener("pagehide", this.#hide);
  }

  #persist(): void {
    if (this.#flushing || this.#timer) return;
    this.#timer = setTimeout(() => this.#flush(), 300);
  }

  #flush(): boolean {
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
    if (this.#forgotten) return false;
    if (this.#unread) {
      try {
        const loaded = load(this.#key);
        const current = this.get();
        if (Object.keys(current.issueDrafts).some((key) => loaded.state.issueDrafts[key])) {
          // A second issue written during the outage must keep the first recoverable.
          try {
            preserveSavedText(localStorage, `${this.#key}.issue-drafts.recovered`, JSON.stringify(loaded.state.issueDrafts));
          } catch {
            this.persistence.set("An earlier issue draft could not be preserved. Keep the app open and retry when storage is available.");
            return false;
          }
          this.recovery.set("An earlier issue draft was kept separately on this device for recovery. New changes can be saved again.");
        }
        const state = mergeRecovered(loaded.state, current);
        this.#unread = false;
        if (loaded.recovered) this.recovery.set(RECOVERED);
        this.#flushing = true;
        try { this.set(state); }
        finally { this.#flushing = false; }
      } catch (error) {
        this.persistence.set(readError(error));
        return false;
      }
    }
    try {
      localStorage.setItem(this.#key, JSON.stringify({ version: VERSION, state: this.get() }));
      this.persistence.set(null);
      return true;
    } catch {
      this.persistence.set("Chats could not be saved on this device. Keep the app open and retry when storage is available.");
      return false;
    }
  }

  /** Write now rather than after the usual pause. False when storage refused. */
  persistNow(): boolean {
    return this.#flush();
  }

  /** Flush work before retiring this pairing, without deleting its drafts. */
  retire(): void {
    this.#flush();
    this.#forgotten = true;
    window.removeEventListener("pagehide", this.#hide);
  }

  /** Stops keeping this computer's interface on the device once it is forgotten. */
  forget(): void {
    this.#forgotten = true;
    this.#flush();
    window.removeEventListener("pagehide", this.#hide);
    localStorage.removeItem(this.#key);
  }

  // ---- Navigation ----------------------------------------------------------

  route(): Route {
    const s = this.get();
    return s.stacks[s.tab].at(-1) ?? ROOT[s.tab];
  }

  /** Switch tab. Selecting the current tab again returns to its root. A
   * tab chosen by hand is a fresh start, so Back no longer leads elsewhere. */
  selectTab(tab: Tab): void {
    // Even the tab already shown: the person has chosen where to be.
    this.#firstRun = false;
    this.set((s) =>
      s.tab === tab ? { ...s, stacks: { ...s.stacks, [tab]: [ROOT[tab]] }, returns: {} } : { ...s, tab, returns: {} },
    );
  }

  /** Once the computer has first said what it holds: on this device's first
   * run, Tasks, when something there needs the person and they haven't gone
   * anywhere yet. Asked once; a later run opens where the person left. */
  openFirst(needsYou: boolean): void {
    if (!this.#firstRun) return;
    const s = this.get();
    const untouched = s.tab === "talk" && s.stacks.talk.length === 1 && !s.taskList &&
      Object.keys(s.taskByProject).length === 0 && Object.keys(s.drafts).length === 0;
    if (needsYou && untouched) this.selectTab("work");
    this.#firstRun = false;
  }

  /** Open a destination on a tab's stack (the current tab by default).
   * Opened from another tab, Back from it goes back to that tab. */
  push(route: Route, tab?: Tab): void {
    if (route.screen === "checklist") return this.openChecklist(route.projectId, route.itemId);
    this.set((s) => {
      const t = tab ?? s.tab;
      const stack = s.stacks[t];
      const top = stack.at(-1);
      const next = top && sameRoute(top, route) ? stack : rooted(t, [...stack, route]);
      if (next === stack && t === s.tab) return s;
      const removed = next === stack ? 0 : Math.max(0, stack.length + 1 - next.length);
      const origin = s.returns[t];
      const returns = t !== s.tab ? { ...s.returns, [t]: { tab: s.tab, depth: next.length } }
        : removed && origin ? { ...s.returns, [t]: { ...origin, depth: Math.max(1, origin.depth - removed) } }
        : s.returns;
      return { ...s, tab: t, stacks: { ...s.stacks, [t]: next }, returns };
    });
  }

  /** Show `route` in place of the screen on top, as the other of two views of
   * one thing, so Back still goes where it went. */
  swap(route: Route): void {
    this.set((s) => {
      const stack = s.stacks[s.tab];
      const top = stack.at(-1);
      if (stack.length < 2 || !top || sameRoute(top, route)) return s;
      return { ...s, stacks: { ...s.stacks, [s.tab]: [...stack.slice(0, -1), route] } };
    });
  }

  /** Return to the nearest matching destination in this tab, or open it. */
  up(route: Route): void {
    const s = this.get(), stack = s.stacks[s.tab];
    const at = stack.findLastIndex((open) => sameRoute(open, route));
    if (at < 0) return this.push(route);
    if (at === stack.length - 1) return;
    // Going above a screen opened from another tab leaves that detour behind.
    // Its return marker must not attach itself to a later screen at that depth.
    const origin = s.returns[s.tab];
    const returns = origin && origin.depth > at + 1 ? without(s.returns, s.tab) : s.returns;
    this.set({ ...s, stacks: { ...s.stacks, [s.tab]: stack.slice(0, at + 1) }, returns });
  }

  /** Show a destination as a tab's root, e.g. a deep link. */
  reset(tab: Tab, routes: Route[] = []): void {
    const checklist = routes.find((route) => route.screen === "checklist");
    if (checklist?.screen === "checklist") return this.openChecklist(checklist.projectId, checklist.itemId);
    this.set((s) => ({ ...s, tab, stacks: { ...s.stacks, [tab]: [ROOT[tab], ...routes] }, returns: without(s.returns, tab) }));
  }

  /** Talk's task list is the app's last step back, as a chat list is. */
  /** `talk` is what Talk's root shows, which only the host's tasks can say. */
  canGoBack(talk: TalkShows = "conversation"): boolean {
    const s = this.get();
    return s.stacks[s.tab].length > 1 || s.tab !== "talk" || (!s.taskList && talk !== "list") || !!s.returns[s.tab];
  }

  /** Step back within the tab, or, at what was opened from elsewhere, back
   * to where it was opened from. */
  back(talk: TalkShows = "conversation"): void {
    this.set((s) => {
      const stack = s.stacks[s.tab];
      const left = leave(s);
      if (left) return left;
      if (stack.length > 1) return { ...s, stacks: { ...s.stacks, [s.tab]: stack.slice(0, -1) } };
      if (s.tab !== "talk") return { ...s, tab: "talk" };
      // As Talk's back arrow: a task goes back to Work, a conversation to the others.
      if (!s.taskList && talk !== "list") return talk === "task" ? { ...listTasks(s), tab: "work" } : listTasks(s);
      return s;
    });
  }

  /** Talk's back arrow: back to where the task was opened from, or else to
   * the project's conversations, or for a task to Work, where tasks are. */
  leaveTask(toWork = false): void {
    this.set((s) => (s.tab === "talk" ? leave(s) : null) ?? (toWork ? { ...listTasks(s), tab: "work" } : listTasks(s)));
  }

  /** Leave the task in Talk for the project's conversations, as a chat's back
   * arrow goes to its chats. The task stays chosen for when Talk returns to it. */
  showTaskList(): void {
    this.set(listTasks);
  }

  /** Finish the originating flow, preserving the selected tab and later navigation. */
  finish(screens: Route["screen"][], tab: Tab = this.get().tab, origin?: Route): void {
    this.set((s) => {
      const stack = s.stacks[tab];
      const end = origin ? stack.indexOf(origin) + 1 : stack.length;
      if (end <= 1) return s;
      let start = end;
      while (start > 1 && screens.includes(stack[start - 1]!.screen)) start--;
      if (start === end) return s;
      // A flow opened from elsewhere that finishes leaves Back leading there.
      const r = s.returns[tab];
      const returns = r && r.depth > start && r.depth <= end ? { ...s.returns, [tab]: { ...r, depth: start } } : s.returns;
      return { ...s, stacks: { ...s.stacks, [tab]: [...stack.slice(0, start), ...stack.slice(end)] }, returns };
    });
  }

  /**
   * Close every Review `match` picks, with the markup that led to it, in
   * whichever tab it was opened. The current tab and later navigation stay.
   */
  closeReviews(match: (route: Extract<Route, { screen: "review" }>) => boolean): void {
    for (const tab of TABS) for (const route of this.get().stacks[tab]) if (route.screen === "review" && match(route)) this.finish(REVIEW_FLOW, tab, route);
  }

  // ---- Takeover handoff ------------------------------------------------------

  // Held outside UiState, so never persisted: a reload must not take control.
  #takeover: { targetId: string; at: number } | null = null;

  /**
   * Take over asked for on a read-only view of a display. The Desktop opened
   * next for that display carries it out, since only Desktop sends input.
   */
  requestTakeover(targetId: string): void {
    this.#takeover = { targetId, at: Date.now() };
  }

  /** Whether a recent takeover is waiting for this display, without claiming it. */
  takeoverRequested(targetId: string): boolean {
    const t = this.#takeover;
    return t?.targetId === targetId && Date.now() - t.at < TAKEOVER_MS;
  }

  /** Local unlock/navigation must discard an unclaimed control intent. */
  cancelTakeover(): void { this.#takeover = null; }

  /** Claims a waiting takeover for this display; true only once. */
  claimTakeover(targetId: string): boolean {
    if (!this.takeoverRequested(targetId)) return false;
    this.#takeover = null;
    return true;
  }

  #source: { annotationId: string; start: SourceStart; at: number } | null = null;

  /** The person chose how to start a drawing, from Attach or an agent's
   * request to capture: the drawing opened next for this annotation starts
   * at the capture choices, or with the files already chosen. */
  requestSource(annotationId: string, start: SourceStart): void {
    this.#source = { annotationId, start, at: Date.now() };
  }

  /** Claims a waiting start for this drawing; returned only once. */
  claimSource(annotationId: string): SourceStart | null {
    const c = this.#source;
    if (c?.annotationId !== annotationId || Date.now() - c.at >= TAKEOVER_MS) return null;
    this.#source = null;
    return c.start;
  }

  // ---- Context ---------------------------------------------------------------

  selectProject(projectId: string): void {
    this.set((s) => (s.projectId === projectId && !s.todoAll ? s : { ...s, projectId, todoAll: false, todoItemId: null }));
  }

  /** Follow work in another project without changing an explicit To-Do All choice. */
  contextProject(projectId: string): void {
    this.set((s) => s.projectId === projectId ? s : { ...s, projectId, todoItemId: null });
  }

  setTodoAll(all: boolean): void {
    this.set((s) => s.todoAll === all ? s : { ...s, todoAll: all, todoItemId: null });
  }

  setTodoItem(itemId: string | null): void {
    this.set((s) => s.todoItemId === itemId ? s : { ...s, todoItemId: itemId });
  }

  /** A checklist link goes to its project in To-Do, never to an overlay. */
  openChecklist(projectId: string, itemId?: string): void {
    this.#firstRun = false;
    this.set((s) => {
      const from: Return | undefined = s.tab !== "todo" ? { tab: s.tab, depth: 1 } : s.returns.todo;
      return { ...s, projectId, todoAll: false, todoItemId: itemId ?? null,
        tab: "todo", stacks: { ...s.stacks, todo: [ROOT.todo] },
        returns: from ? { ...s.returns, todo: from } : without(s.returns, "todo") };
    });
  }

  /** Show a task (or null for a new one) in Talk for its project. Choosing
   * another task in Talk starts afresh, so Back no longer leads elsewhere. */
  selectTask(projectId: string, taskId: string | null): void {
    this.set((s) => {
      const next = choose(s, projectId, taskId);
      return next.stacks === s.stacks && s.projectId === projectId && s.taskByProject[projectId] === taskId ? next : { ...next, returns: without(s.returns, "talk") };
    });
  }

  /** Start writing a new conversation in Talk's project. */
  newConversation(projectId: string): void {
    this.set((s) => ({ ...choose(s, projectId, null), tab: "talk", conversing: true, returns: without(s.returns, "talk") }));
  }

  /** Open a task in Talk from wherever the person is; Back from it returns there. */
  openTask(projectId: string, taskId: string | null): void {
    this.set((s) => {
      const next = choose(s, projectId, taskId);
      const from: Return | undefined = s.tab !== "talk" ? { tab: s.tab, depth: 1 }
        : s.taskList ? s.returns.talk
        : s.projectId && (s.projectId !== projectId || s.taskByProject[s.projectId] !== taskId)
          ? { tab: "talk", depth: 1, task: { projectId: s.projectId, taskId: s.taskByProject[s.projectId] ?? null } }
          : s.returns.talk;
      return { ...next, tab: "talk", stacks: { ...next.stacks, talk: [ROOT.talk] }, returns: from ? { ...s.returns, talk: from } : without(s.returns, "talk") };
    });
  }

  /** Show a task this device just created. Its receipt can arrive a moment
   * before the computer's record of the task, so Talk waits briefly for it
   * instead of calling it removed. Back from it still leads to wherever its
   * Review was opened from. */
  selectCreated(projectId: string, taskId: string): void {
    this.#created.set(taskId, Date.now());
    this.set((s) => {
      // A conversation just sent stays one while it arrives.
      const next = { ...choose(s, projectId, taskId), conversing: s.conversing };
      const r = s.returns.talk;
      return { ...next, returns: r ? { ...s.returns, talk: { ...r, depth: next.stacks.talk.length } } : s.returns };
    });
  }

  /** Milliseconds Talk should still wait for a task this device created; 0 once the wait is over. */
  arrivalWait(taskId: string): number {
    const at = this.#created.get(taskId);
    return at === undefined ? 0 : Math.max(0, at + ARRIVAL_MS - Date.now());
  }

  selectTarget(targetId: string | null): void {
    this.set((s) => ({ ...s, targetId }));
  }

  // ---- Drafts ----------------------------------------------------------------

  draft(projectId: string, taskId: string | null): Draft | null {
    return this.get().drafts[draftKey(projectId, taskId)] ?? null;
  }

  /** Update the unsent text and/or attachment; an empty draft is removed. */
  saveDraft(projectId: string, taskId: string | null, patch: Partial<Pick<Draft, "text" | "annotationId" | "agent" | "mode" | "access" | "attempt" | "review">>): void {
    const key = draftKey(projectId, taskId);
    this.set((s) => {
      const drafts = { ...s.drafts };
      const prev = drafts[key];
      // Undefined in the patch clears it, as nothing chosen is the default.
      const access = "access" in patch ? patch.access : prev?.access;
      const next: Draft = {
        projectId,
        taskId,
        text: patch.text ?? prev?.text ?? "",
        annotationId: patch.annotationId !== undefined ? patch.annotationId : (prev?.annotationId ?? null),
        agent: patch.agent !== undefined ? patch.agent : (prev?.agent ?? null),
        mode: patch.mode ?? prev?.mode,
        ...(access ? { access } : {}),
        attempt: patch.attempt !== undefined ? patch.attempt : (prev?.attempt ?? null),
        review: patch.review !== undefined ? patch.review : (prev?.review ?? null),
        updatedAt: Date.now(),
      };
      if (next.text.trim() === "" && next.annotationId === null && !next.agent && !next.access && !next.attempt && !hasReviewDetails(next.review)) delete drafts[key];
      else drafts[key] = next;
      return { ...s, drafts };
    });
  }

  clearDraft(projectId: string, taskId: string | null): void {
    this.set((s) => {
      const drafts = { ...s.drafts };
      delete drafts[draftKey(projectId, taskId)];
      return { ...s, drafts };
    });
  }

  /**
   * Update the issue being written; one back as New issue opens is removed.
   * Given `only`, it changes only the draft with that id, and says whether
   * it did.
   */
  saveIssueDraft(projectId: string, patch: Partial<Omit<IssueDraft, "id" | "updatedAt">>, only?: string): boolean {
    if (only !== undefined && this.get().issueDrafts[projectId]?.id !== only) return false;
    this.set((s) => {
      const prev = s.issueDrafts[projectId];
      const next: IssueDraft = {
        title: "", body: "", then: "start", agent: null, reviewOn: false, reviewer: null, filed: null, pending: null,
        ...prev, ...patch, id: prev?.id ?? uuidv7(), updatedAt: Date.now(),
      };
      const issueDrafts = { ...s.issueDrafts };
      if (!next.title.trim() && !next.body.trim() && next.then === "start" && !next.agent && !next.reviewOn && !next.reviewer && !next.filed && !next.pending)
        delete issueDrafts[projectId];
      else issueDrafts[projectId] = next;
      return { ...s, issueDrafts };
    });
    return true;
  }

  /** Given `only`, it clears only the draft with that id. */
  clearIssueDraft(projectId: string, only?: string): void {
    this.set((s) => {
      if (!s.issueDrafts[projectId] || (only !== undefined && s.issueDrafts[projectId].id !== only)) return s;
      const issueDrafts = { ...s.issueDrafts };
      delete issueDrafts[projectId];
      return { ...s, issueDrafts };
    });
  }
}

export function sameRoute(a: Route, b: Route): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}
