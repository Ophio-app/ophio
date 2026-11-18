// SPDX-License-Identifier: Apache-2.0
//
// Work a chat's agent suggests, as one card in the chat (Design A). Each
// suggestion is started as the same task the Tasks tab would make for it,
// with the least access that work needs, and its card says exactly that
// before the tap: the kind of task, its branch and pull request, what it may
// reach and when it starts. What starts is what was shown: the computer
// compares the task against the suggestion it keeps (`origin`) and refuses
// anything that differs, so neither this phone nor the agent can widen it.
//
// The kind and the access come from the suggestion. The agent, model, effort
// and permission prompts are the person's usual ones for that kind of task
// (`launchMemory`), or what they pick on the card; nothing the agent writes
// in a suggestion can set them.

import type { AgentChoice } from "../../app/ui";
import type {
  Command, ConversationAccess, GithubLink, GithubProjectView, HarnessInfo, LaunchChoice, LaunchPolicy, NewWork, Project, TaskMode, WorkNeeds,
} from "../../protocol/types";
import { defaultLaunchPolicy, launchChoices } from "../../app/launchPolicy";
import { agentChoiceBlocker, agentLabel, createTaskCommand, modeSupport } from "./intent";
import type { WorkKind } from "../../app/launchMemory";

export const WORK_CARDS_FEATURE = "chat_work_cards_v1";
/** The computer tells a chat how the work it started ended, and makes a task
 * from a chat while the chat stays. */
export const HEARS_BACK_FEATURE = "chat_hears_back_v1";
/** Ordered sets of suggestions, a card for Run all, and a project's go-ahead
 * for what its chats' agents propose. */
export const SETS_FEATURE = "chat_work_sets_v1";
/** A card opened in Review and started there stays that card's task. */
export const REVIEWED_FEATURE = "chat_work_reviewed_v1";

/** What a suggestion is, from the work itself. */
export type CardKind = WorkKind;

/** A suggestion as the computer keeps it. One from an older computer has
 * neither `needs` nor `refused`. */
export type Suggestion = NewWork;

export function cardKind(w: Suggestion): CardKind {
  if (w.needs?.desktop) return "desktop";
  if (w.issue != null) return "fix";
  return w.mode === "ask" ? "find" : w.mode === "plan" ? "plan" : "change";
}

/** The task mode each kind starts in. An issue is fixed in Work, or planned first. */
export function kindMode(k: CardKind, w: Pick<Suggestion, "mode">): TaskMode {
  return k === "find" ? "ask" : k === "plan" || (k === "fix" && w.mode === "plan") ? "plan" : "work";
}

// As the computer compares names: the same in every locale, so a phone set to
// Turkish still finds "INDEX" by "index".
const lower = (s: string) => s.trim().toLowerCase();

/** The one project named `name`, if exactly one is. */
function byName(name: string, projects: Project[]): Project | null {
  const found = projects.filter((p) => !p.one_off && lower(p.name) === lower(name));
  return found.length === 1 ? found[0]! : null;
}

export interface Places {
  /** Where the task runs: the chat's project unless the suggestion names another. */
  target: Project | null;
  /** Other projects it reads. */
  read: Project[];
  /** Names no single project on this computer has. */
  unknown: string[];
}

export function places(w: Suggestion, chat: Project, all: Record<string, Project | undefined>): Places {
  const list = Object.values(all).filter((p): p is Project => !!p);
  const unknown: string[] = [];
  const changes = w.needs?.changes?.trim();
  const target = changes ? byName(changes, list) : chat;
  if (changes && !target) unknown.push(changes);
  const read: Project[] = [];
  for (const name of w.needs?.read ?? []) {
    const p = byName(name, list);
    if (!p) unknown.push(name);
    else if (p.project_id !== target?.project_id && !read.some((r) => r.project_id === p.project_id)) read.push(p);
  }
  return { target, read, unknown };
}

/**
 * The least a question needs: its project and what else it names to read,
 * nothing to change, and the internet or the person's own network only when
 * it says so. Reaching the own network goes through the internet, as the
 * computer holds it. The computer derives the same and refuses any other.
 */
export function leastAccess(w: Suggestion, p: Places): ConversationAccess | null {
  if (cardKind(w) !== "find" || !p.target) return null;
  const n: Partial<WorkNeeds> = w.needs ?? {};
  const local = !!n.local_network;
  return {
    read: [p.target.project_id, ...p.read.map((r) => r.project_id)],
    write: [],
    whole_read: !!n.whole_computer,
    whole_write: false,
    internet: !!n.internet || local,
    local_network: local,
  };
}

/** Short words for a permission setting, said on the card. */
const POLICY_WORDS: Record<LaunchPolicy, string> = {
  standard: "asks before risky steps",
  accept_edits: "edits without asking",
  sandboxed: "sandboxed, no prompts",
  bypass_permissions: "no permission prompts",
};

export interface Habits {
  agent: AgentChoice;
  /** Whether Work gets its own branch and pull request where GitHub is on. */
  pr: boolean;
}

export interface CardInput {
  w: Suggestion;
  index: number;
  chatId: string;
  runId: string;
  chat: Project;
  projects: Record<string, Project | undefined>;
  habits: Habits | null;
  harnesses: HarnessInfo[];
  /** The target project's GitHub view: undefined while it is read, null where GitHub isn't set up. */
  github: GithubProjectView | null | undefined;
  /** The project's reviewer for issue fixes, if it has one. */
  reviewer: LaunchChoice | null;
  /** Whether Work in the target project would wait for another task: its title. */
  writer: { title: string | null } | null;
  /** Whether the person paused the target project's Next up. A card never resumes it. */
  paused: boolean;
  features: readonly string[];
  /** Whether this phone may start tasks and is connected. */
  canStart: boolean;
  signInOn?: string | null;
  /** In an ordered set: it goes through Next up, after the one before it. */
  inSet?: boolean;
  /** The title of the task before it in its set. */
  follows?: string | null;
}

interface Shown {
  kind: CardKind;
  /** The kind of task, its route, what it may reach and when it starts. */
  line: string;
  /** The line without when it starts: what an ordered set says once when its tasks share it. */
  what: string;
  /** Anything past its own project, always written out. */
  beyond: string[];
  /** The agent, model and effort it starts with, as the chip says them. */
  chip: string;
}

export type CardPlan =
  | (Shown & { start: Command; queued: boolean; review: null; refused: null })
  /** Start goes through Review, or a screen of its own, and why. */
  | (Shown & { start: null; queued: boolean; review: string; refused: null })
  /** The suggestion can't start as written; it can still be opened in Review. */
  | (Shown & { start: null; queued: false; review: null; refused: string });

const list = (names: string[]): string => new Intl.ListFormat("en", { type: "conjunction" }).format(names);

/**
 * What one suggestion starts as, in words for its card and as the command its
 * Start sends, or why Start opens Review instead.
 */
export function planCard(i: CardInput): CardPlan {
  const { w } = i;
  const kind = cardKind(w);
  const p = places(w, i.chat, i.projects);
  const n: Partial<WorkNeeds> = w.needs ?? {};
  const mode = kindMode(kind, w);
  const policy = i.habits?.agent.launchPolicy ?? null;
  const githubOn = !!i.github && i.github.settings.mode !== "off";
  const pr = (kind === "fix" && mode === "work") || (kind === "change" && githubOn && !!i.habits?.pr);

  // What it may reach, said in full on the card: anything past its own
  // project is never behind an opener.
  const beyond: string[] = [];
  if (p.target && p.target.project_id !== i.chat.project_id) beyond.push(`works in ${p.target.name}`);
  if (p.read.length) beyond.push(`reads ${list(p.read.map((r) => r.name))}`);
  if (n.whole_computer) beyond.push("reads the whole computer");
  if (n.local_network) beyond.push("reaches your own network");
  if (n.desktop) beyond.push("uses the desktop");
  if (mode === "work" && policy === "bypass_permissions" && i.habits && defaultLaunchPolicy(i.habits.agent.harness) !== "bypass_permissions")
    beyond.push("no permission prompts");

  const kindWords = kind === "find" ? "Find out" : kind === "plan" ? "Plan" : kind === "fix" ? `${mode === "plan" ? "Plan a fix for" : "Fix"} issue #${w.issue}` : kind === "desktop" ? "On the desktop" : "Change";
  const route = kind === "find" ? "read-only"
    : kind === "plan" || mode === "plan" ? "read-only, then you decide"
    : kind === "desktop" ? "you choose the screen first"
    : pr ? (kind === "fix" && i.reviewer ? "own branch and pull request, reviewed" : "own branch and pull request")
    : "changes the project folder";
  const reach = kind === "find"
    // Your own network comes with the internet (leastAccess grants both), so
    // the card says the internet whenever either is asked for.
    ? (p.read.length || n.whole_computer ? null : n.internet || n.local_network ? "this project only, uses the internet" : "this project only, no internet")
    : kind === "desktop" ? null
    : mode === "plan" ? (p.target && p.target.project_id !== i.chat.project_id ? null : "this project")
    : policy ? POLICY_WORDS[policy] : "the agent’s own permission settings";
  const queued = kind !== "desktop" && (!!i.inSet || (mode === "work" && i.writer !== null));
  // Starting by itself from Next up would resume all of it, so while the
  // person has it paused the task waits there for their own Start.
  const held = queued && !!i.paused;
  const when = kind === "desktop" ? null
    : held ? "waits in Next up, which is paused, until you start it"
    : i.follows ? `starts after “${i.follows}”`
    : queued ? (i.writer?.title ? `starts after “${i.writer.title}”` : "waits in Next up, starts by itself") : "starts now";
  const what = [kindWords, route, reach].filter(Boolean).join(" · ");
  const line = [what, when].filter(Boolean).join(" · ");
  const chip = i.habits ? agentLabel(i.harnesses, i.habits.agent) : "Choose an agent";

  const refused = w.refused ?? (p.unknown.length ? `It names ${p.unknown.length === 1 ? "a project" : "projects"} this computer doesn’t have: ${list(p.unknown)}.` : null);
  if (refused) return { kind, start: null, line: kindWords, what: kindWords, beyond, chip, queued: false, review: null, refused };

  const review = (why: string): CardPlan => ({ kind, start: null, line, what, beyond, chip, queued, review: why, refused: null });
  if (kind === "desktop") return review("Work on the desktop starts once you choose the screen.");
  if (p.target!.project_id !== i.chat.project_id) return review(`It works in ${p.target!.name}, so it starts from Review.`);
  if (!i.features.includes(WORK_CARDS_FEATURE)) return review("This computer’s Øphio is older, so Start opens Review.");
  if (!i.canStart) return review("Start opens Review while this phone can’t start tasks.");
  const h = i.habits;
  if (!h) return review("Choose an agent first.");
  const harness = i.harnesses.find((x) => x.kind === h.agent.harness);
  const blocker = agentChoiceBlocker(harness, h.agent, i.signInOn);
  if (blocker) return review(blocker);
  if (modeSupport(harness, mode).availability.state !== "available") return review(`${harness?.display_name ?? "This agent"} can’t do this kind of task.`);
  if (h.agent.launchPolicy != null && !i.features.includes("launch_policy_v1")) return review("This computer can’t take the permission setting.");
  if (kind === "fix" || (kind === "change" && h.pr)) {
    if (i.github === undefined) return review("Checking the project’s GitHub setting…");
    if (kind === "fix" && !githubOn) return review("Turn on GitHub for this project to fix the issue there.");
    // The through-merge warning is a confirmation of its own: answered in Review.
    if (githubOn && i.github!.settings.mode === "through_merge" && !i.github!.settings.warning_answered) return review("This project merges by itself. Confirm that in Review first.");
  }
  const github: GithubLink | null = kind === "fix" ? { type: "issue", number: w.issue!, ...(i.reviewer ? { review_with: i.reviewer } : {}) }
    : pr ? { type: "new_pr" } : null;
  const access = leastAccess(w, p);
  const base = createTaskCommand({
    projectId: p.target!.project_id,
    prompt: w.brief,
    title: w.title,
    agent: h.agent,
    mode,
    placement: queued ? { mode: "queue", position: null, authorization: held ? "ask_before_start" : "auto_start" } : { mode: "start_now" },
    mission: null,
    bundle: null,
    branch: null,
    start: null,
    place: null,
    browser: null,
    github,
    ...(access ? { access } : {}),
  });
  // The computer compares exactly these words with the suggestion it keeps.
  const start = { ...base, prompt: w.brief, title: w.title, origin: { chat: i.chatId, run: i.runId, index: i.index } } as Command;
  return { kind, start, line, what, beyond, chip, queued, review: null, refused: null };
}

/** A turn's suggestions as cards: those that stand alone, each ordered set
 * (by position, first to last), and each proposal to fix every issue. A
 * suggestion whose `after` isn't the last of a set stands alone; the
 * computer still starts it only after the one it names. */
export interface Groups { loose: number[]; sets: number[][]; allIssues: number[] }

export function groups(work: Suggestion[]): Groups {
  const out: Groups = { loose: [], sets: [], allIssues: [] };
  const chains: number[][] = [];
  work.forEach((w, i) => {
    if (w.all_issues) { out.allIssues.push(i); return; }
    const chain = w.after != null ? chains.find((c) => c.at(-1) === w.after) : undefined;
    if (chain) chain.push(i); else chains.push([i]);
  });
  for (const c of chains) {
    if (c.length > 1) out.sets.push(c); else out.loose.push(c[0]!);
  }
  out.loose.sort((a, b) => a - b);
  return out;
}

/** Whether a card's suggestions ask for more than usual: never started without the person. */
export const asksMore = (c: Pick<CardPlan, "beyond">): boolean => c.beyond.length > 0;

/** The words for a permission setting, for tests and the summary line. */
export const policyWords = (harness: AgentChoice["harness"], policy: LaunchPolicy | null | undefined): string =>
  policy ? (launchChoices(harness).some((c) => c.value === policy) ? POLICY_WORDS[policy] : "an unknown permission setting") : "the agent’s own permission settings";

export interface TypedInput {
  /** What the person typed. */
  text: string;
  project: Project;
  agent: AgentChoice | null;
  harnesses: HarnessInfo[];
  /** The project's GitHub view, as Review reads it: undefined while it is read. */
  github: GithubProjectView | null | undefined;
  /** False once the person took the pull request off this draft. */
  pr: boolean;
  writer: { title: string | null } | null;
  /** Whether Next up is paused, so a queued start would wait for the person. */
  paused: boolean;
  features: readonly string[];
  canStart: boolean;
  signInOn?: string | null;
}

/**
 * A task typed in the message box, started from there with the settings
 * Review would show for it untouched: the person's usual agent for Work, its
 * own branch and pull request where GitHub is on, now or in Next up. When
 * anything there would ask the person something, or reaches past the project,
 * Send opens Review as before. The computer holds it to its ordinary rules.
 */
export function planTyped(i: TypedInput): CardPlan {
  const words = i.text.trim();
  const plan = planCard({
    w: { title: words.split("\n")[0]!.slice(0, 80), brief: words, mode: "work" },
    index: 0, chatId: "", runId: "", chat: i.project, projects: { [i.project.project_id]: i.project },
    habits: i.agent ? { agent: i.agent, pr: i.pr } : null, harnesses: i.harnesses,
    github: i.github, reviewer: null, writer: i.writer, paused: i.paused, features: i.features, canStart: i.canStart, signInOn: i.signInOn ?? null,
  });
  if (!plan.start) return plan;
  const review = (why: string): CardPlan => ({ ...plan, start: null, review: why });
  if (!words) return review("Write what you want done first.");
  if (asksMore(plan)) return review("It asks for more than usual, so it starts from Review.");
  if (plan.queued && i.paused) return review("Next up is paused, so it starts from Review.");
  // As Review sends it: no title of its own, and the agent browser where the computer has it.
  const { origin: _origin, ...rest } = plan.start as Extract<Command, { command: "create_task" }>;
  const start = { ...rest, title: null, ...(i.features.includes("agent_logins_v1") ? { browser: true } : {}) } as Command;
  return { ...plan, start };
}
