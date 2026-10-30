// SPDX-License-Identifier: Apache-2.0
//
// The GitHub workflow as the phone shows it: its words, what a task's link
// means, and the computer's reads of a project's repository, issues and pull
// requests. The computer does every GitHub read and write with the account
// signed in under Accounts; the phone only asks and shows.

import { useCallback, useEffect, useMemo, useState } from "react";
import { describeError, shallowEqual, useApp, useHost, useLink } from "./context";
import { commandResult } from "../host/commandResult";
import type { HostSession } from "../host/session";
import { GITHUB_FEATURE, GITHUB_ISSUES_FEATURE, GITHUB_REVIEW_FEATURE, GITHUB_UAT_FEATURE } from "./githubFeature";
import { defaultLaunchPolicy } from "./launchPolicy";
import { draftKey, freshReview, type AgentChoice, type UiStore } from "./ui";
import type {
  CheckState, CommandResult, FindFocus, FindScope, FixFindings, GithubAction, GithubItem, GithubItemKind, GithubItems, GithubLink,
  GithubMode, GithubPrView, GithubProjectView, ItemStatus, LaunchChoice, MergeMethod, PrStage, PrSummary, TaskGithub,
  TaskMode, TriageGroup,
} from "../protocol/types";
import { itemStatus, linkedTask } from "../state/githubStatus";
import { projectTasks } from "../state/projection";
import { Store, useStore } from "../state/store";


/** Merging waits for GitHub to answer, then for the computer to read it back. */
const ACT_WAIT_MS = 60_000;

export const MODE_HINT: Record<GithubMode, string> = {
  off: "Tasks aren’t linked to GitHub.",
  through_merge: "The agent opens a draft pull request, keeps its checks passing, then marks it ready and merges it.",
  drafts: "The agent opens a draft pull request and keeps its checks passing. You mark it ready and merge it.",
};
export const FIX_WORDS: Record<FixFindings, string> = { by_itself: "Fix by itself", ask_first: "Queue fixes", file_only: "Only file" };
/** What happens to the issues a review files. */
export const FIX_HINT: Record<FixFindings, string> = {
  by_itself: "The fixer checks each issue is real, fixes it and opens a pull request, one after another, without waiting for you.",
  ask_first: "Each issue becomes “Fix #N” in Next up with the fixer, and waits for you to start it.",
  file_only: "The issues are filed and nothing else happens. Start the ones you want from Issues.",
};
export const STAGE_WORDS: Record<PrStage, string> = {
  babysitting: "Watching checks", ready: "Ready", checking: "Being checked", merging: "Waiting to merge", merged: "Merged",
  capped: "Needs you", not_reproduced: "Couldn’t reproduce", closed: "Closed",
};
export const STATUS_WORDS: Record<ItemStatus, string> = { agent_working: "Agent working", waiting_for_you: "Waiting for you", green: "Checks pass", queued: "In Next up", stopped: "Stopped" };
export const GROUP_WORDS: Record<TriageGroup, string> = { agent_can_take: "Agent can take it", needs_you: "Needs you" };
/** Review profiles, in the order they're offered; the first four are the ones older computers know. */
export const FOCUSES: FindFocus[] = ["bugs", "security", "tests", "accessibility", "ui_ux", "performance", "reliability", "maintainability", "dependencies", "privacy", "api", "docs"];
const OLD_FOCUSES: FindFocus[] = ["bugs", "security", "tests", "accessibility"];
/** Reviewers who use the app on a screen instead of reading its code. */
export const USERS: FindFocus[] = ["everyday_user", "novice_user", "power_user"];
/** Whether a profile uses the app, as computer use, rather than reading the code. */
export const usesIt = (focus: FindFocus | null | undefined): boolean => !!focus && USERS.includes(focus);
/** The profiles a computer can run: all of them, or the first four. */
export const focusesFor = (reviewSupported: boolean): FindFocus[] => (reviewSupported ? FOCUSES : OLD_FOCUSES);
/** Each profile by the reviewer it makes the agent. */
export const FOCUS_WORDS: Record<FindFocus, string> = {
  bugs: "Bug hunter", security: "Security researcher", tests: "Test engineer", accessibility: "Accessibility specialist",
  ui_ux: "UI/UX developer", performance: "Performance engineer", reliability: "Reliability engineer",
  maintainability: "Code quality", dependencies: "Dependency auditor", privacy: "Privacy reviewer", api: "API reviewer",
  docs: "Docs reviewer", everyday_user: "Everyday user", novice_user: "Novice user", power_user: "Power user",
};
/** The issues a profile looks for, as a review's request names them. */
export const FOCUS_ISSUES: Record<FindFocus, string> = {
  bugs: "bugs", security: "security", tests: "test", accessibility: "accessibility", ui_ux: "UI/UX", performance: "performance",
  reliability: "reliability", maintainability: "code quality", dependencies: "dependency", privacy: "privacy", api: "API", docs: "docs",
  everyday_user: "everyday user", novice_user: "novice user", power_user: "power user",
};
/** What each profile looks for, in a line; the computer gives the agent the full brief. */
export const FOCUS_HINT: Record<FindFocus | "default", string> = {
  default: "A careful senior engineer: bugs, security problems, missing tests, accessibility and anything else clearly wrong.",
  bugs: "Logic errors, edge cases, unhandled errors, races, and behaviour that breaks what the code or its docs promise.",
  security: "Injection, broken auth, leaked secrets, path traversal, SSRF, unsafe defaults: only what an attacker could reach.",
  tests: "Important behaviour with no test, tests that can’t fail, flaky tests and fixtures that hide bugs.",
  accessibility: "Labels and roles, keyboard and screen reader paths, focus, contrast, target size and text size, by WCAG criterion.",
  ui_ux: "Confusing flows, missing loading, empty and error states, unclear controls, layouts that break, and lost input.",
  performance: "Repeated work, N+1 queries, blocking calls, unbounded memory and slow startup, measured at realistic sizes.",
  reliability: "Crashes, swallowed errors, lost or corrupted data, missing timeouts, and failures part way through.",
  maintainability: "Drifting duplicates, dead code, misleading names and abstractions that make changes risky. Not style.",
  dependencies: "Vulnerable or abandoned packages, loose versions, install scripts and build steps that run unchecked code.",
  privacy: "Personal data collected, logged, shared or kept longer than needed, unencrypted, or impossible to delete.",
  api: "Endpoints, functions and formats that don’t match their docs or types, breaking changes and missing validation.",
  docs: "Instructions that no longer work, missing setup steps, and behaviour nobody wrote down.",
  everyday_user: "Uses the app on its screen like a typical person: unclear steps, surprises, dead ends and anything that feels broken.",
  novice_user: "Uses the app on its screen as someone who struggles with technology, and gets upset by jargon, hidden controls and anything not simple.",
  power_user: "Uses the app on its screen as a fast, technical person, and gets annoyed by extra clicks, needless confirmations and missing shortcuts.",
};
/** The most issues one review files. */
export const MAX_ISSUES = 20;
export const METHOD_WORDS: Record<MergeMethod, string> = { squash: "Squash and merge", merge: "Merge commit", rebase: "Rebase and merge" };
export const CHECK_WORDS: Record<CheckState, string> = { pending: "Running", passed: "Passed", failed: "Failed", other: "Didn’t run" };

/** What merging once checks pass means, shown before it can merge anything. */
export function warningText(branch: string, repo: string): string {
  return `The agent will merge its own pull requests into ${branch} on ${repo} when every check passes and no review comment is open, without asking you first. A merge can be undone with a revert, but anything that deploys from ${branch} deploys it.`;
}

/** The switch's reminder for as long as the agent merges by itself. */
export const reminderText = (branch: string) => `The agent merges its pull requests into ${branch} once their checks pass, without asking.`;

/** Find, sort and check report and change no file; all but a Find that uses the app run in Plan. */
export const readOnlyLink = (link: GithubLink | null | undefined): boolean =>
  link?.type === "find" || link?.type === "triage" || link?.type === "check";

/** A Find that uses the app, which runs as computer use. */
export const usesItLink = (link: GithubLink | null | undefined): boolean => link?.type === "find" && usesIt(link.focus);

/** Why a read-only link runs only in Plan. */
export function planOnlyWhy(link: GithubLink): string {
  if (link.type === "find") return "Find only reads, so it runs in Plan. The fixing happens in Work: each finding is filed as an issue and its fix is queued in Next up.";
  return `${link.type === "triage" ? "Sorting issues" : "Check"} only reads, so it runs in Plan.`;
}

/** Why a read-only link won't start in Work. */
export function planOnlyText(link: GithubLink): string {
  return `${planOnlyWhy(link)} ${link.type === "find" ? "Choose Plan." : "Choose Plan, or another GitHub link."}`;
}

export function scopeWords(scope: FindScope): string {
  switch (scope.scope) {
    case "project": return "The whole project";
    case "folder": return `The folder ${scope.path}`;
    case "since_last_run": return "Commits since its last run";
    case "pr": return `Pull request #${scope.number}`;
  }
}

/** One short name for a link, as Review's GitHub row shows it. */
export function linkWords(link: GithubLink | null): string {
  if (!link) return "Not linked";
  switch (link.type) {
    case "issue": return `Issue #${link.number}`;
    case "pr": return `Pull request #${link.number}`;
    case "new_pr": return "New pull request";
    case "stack": return `Stack on PR #${link.number}`;
    case "find": return `Find issues${link.focus ? ` · ${FOCUS_WORDS[link.focus]}` : ""}`;
    case "triage": return "Sort issues";
    case "check": return `Check #${link.number}`;
  }
}

const STRANGER = "If someone who can’t write to the repository wrote it, it starts in Plan.";

/** What the link has the agent do, under its name in Review. */
export function linkDetail(link: GithubLink | null, branch: string | null): string {
  if (!link) return "The agent doesn’t open a pull request or post on GitHub.";
  switch (link.type) {
    case "issue": return `Takes the issue’s title. The agent makes a new branch, opens a draft pull request and says “Closes #${link.number}” only if its fix covers the issue. ${STRANGER}`;
    case "pr": return `Continues the pull request’s own branch, brings it up to date with a merge, and keeps its checks passing. A fork’s branch it can’t push to gets a replacement pull request that credits it. ${STRANGER}`;
    case "new_pr": return `The agent pushes ${branch ? branch : "its branch"} and opens a draft pull request early, then keeps its checks passing.`;
    case "stack": return `The new branch starts from #${link.number}’s, and its pull request targets it. When #${link.number} merges, GitHub moves this one onto the main branch.`;
    case "find": {
      const most = link.max_issues ? `at most ${plural(link.max_issues, "issue")}` : "an issue for each finding";
      const then = link.fix_findings === "file_only" ? " Nothing else happens until you start a fix."
        : link.fix_findings === "ask_first" ? " Each becomes a “Fix #N” task in Next up that waits for you."
          : " Each becomes a “Fix #N” task in Next up, which runs in Work.";
      if (usesIt(link.focus)) {
        const what = link.scope.scope === "since_last_run" ? "what changed since its last run" : "the whole app";
        return `Uses ${what} on a screen as ${article(FOCUS_ISSUES[link.focus!])}, without changing the project, and files ${most}.${then}`;
      }
      return `Reads ${scopeWords(link.scope).toLowerCase()} in Plan, without changing files, and files ${most}.${then}`;
    }
    case "triage": return "Reads the open issues and pull requests in Plan and sorts them into what the agent can take and what needs you. Nothing starts by itself.";
    case "check": return `Reviews #${link.number}’s changes in Plan, without changing files, and reports back.`;
  }
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const article = (who: string) => `${/^[aeiou]/i.test(who) ? "an" : "a"} ${who}`;

/** A pull request at a glance: its number, draft or ready, checks and threads. */
export function prWords(pr: PrSummary): string[] {
  const parts = [`PR #${pr.number}`];
  parts.push(pr.state === "merged" ? "Merged" : pr.state === "closed" ? "Closed" : pr.draft ? "Draft" : "Ready");
  if (pr.state === "open") {
    if (pr.failed) parts.push(`${pr.failed} failing`);
    if (pr.pending) parts.push(`${pr.pending} running`);
    if (!pr.failed && !pr.pending && pr.passed) parts.push("Checks pass");
    if (pr.unresolved) parts.push(plural(pr.unresolved, "open thread"));
    if (pr.conflict) parts.push("Conflict");
  }
  return parts;
}

/** The task's PR summary, while it has one. */
export const taskPr = (github: TaskGithub | undefined): PrSummary | null => github?.pr ?? null;

/** The agent, model and effort a run used, as a link carries it. */
export const choiceOf = (c: { harness: LaunchChoice["harness"]; model: string | null; effort: string | null }): LaunchChoice =>
  ({ harness: c.harness, model: c.model, effort: c.effort });

/**
 * Who fixes, with the launch flags chosen for the fixes, if any. Where the
 * sheet couldn't offer launch flags, the fixes keep the ones they had for the
 * same agent, so the phone's defaults aren't remembered as a choice.
 */
export function fixChoiceOf(a: AgentChoice, flagsOffered: boolean, before: LaunchChoice): LaunchChoice {
  const launch = flagsOffered ? a.launchPolicy : a.harness === before.harness ? before.launch_policy : null;
  return { ...choiceOf(a), ...(launch ? { launch_policy: launch } : {}) };
}

/**
 * A Find's fixes as sent: their launch flags as chosen, else this phone's
 * launch defaults, and none to a computer that can't apply them.
 */
export function fixSent(fix: LaunchChoice, supported: boolean): LaunchChoice {
  const launch = supported ? fix.launch_policy ?? defaultLaunchPolicy(fix.harness) : null;
  return { ...choiceOf(fix), ...(launch ? { launch_policy: launch } : {}) };
}

/** Who reviews, who fixes, who checks an issue's fix, and who uses the app. */
type Role = "find" | "fix" | "review" | "use";
const roleKey = (profile: string) => `github.roles.${profile}`;

/** A project's last agent in a role, remembered on this phone. */
export function lastRole(profile: string, projectId: string, role: Role): LaunchChoice | null {
  try {
    const all = JSON.parse(localStorage.getItem(roleKey(profile)) ?? "{}") as Record<string, Partial<Record<Role, LaunchChoice>>>;
    return all[projectId]?.[role] ?? null;
  } catch { return null; }
}

export function rememberRole(profile: string, projectId: string, role: Role, choice: LaunchChoice): void {
  try {
    const all = JSON.parse(localStorage.getItem(roleKey(profile)) ?? "{}") as Record<string, Partial<Record<Role, LaunchChoice>>>;
    all[projectId] = { ...all[projectId], [role]: { ...choiceOf(choice), ...(choice.launch_policy ? { launch_policy: choice.launch_policy } : {}) } };
    localStorage.setItem(roleKey(profile), JSON.stringify(all));
  } catch { /* The choice still goes with this task. */ }
}

const limitKey = (profile: string) => `github.limits.${profile}`;

/** The most issues last set for a project's reviews on this phone, if any. */
export function lastLimit(profile: string, projectId: string): number | null {
  try {
    const n = (JSON.parse(localStorage.getItem(limitKey(profile)) ?? "{}") as Record<string, unknown>)[projectId];
    return typeof n === "number" && Number.isInteger(n) ? Math.min(Math.max(n, 1), MAX_ISSUES) : null;
  } catch { return null; }
}

export function rememberLimit(profile: string, projectId: string, most: number): void {
  try {
    const all = JSON.parse(localStorage.getItem(limitKey(profile)) ?? "{}") as Record<string, number>;
    localStorage.setItem(limitKey(profile), JSON.stringify({ ...all, [projectId]: most }));
  } catch { /* The number still goes with this review. */ }
}

/** Whether the computer links tasks to GitHub; unknown capabilities read as no. */
export const useGithubSupported = () => useHost((s) => !!s.capabilities?.features.includes(GITHUB_FEATURE));

/** Whether the computer takes a review's profile, limit and what follows. */
export const useGithubReviewSupported = () => useHost((s) => !!s.capabilities?.features.includes(GITHUB_REVIEW_FEATURE));

/** Whether the computer files issues the person writes and gives an issue its own reviewer. */
export const useGithubIssuesSupported = () => useHost((s) => !!s.capabilities?.features.includes(GITHUB_ISSUES_FEATURE));

/** Whether the computer has reviewers who use the app on a screen. */
export const useGithubUatSupported = () => useHost((s) => !!s.capabilities?.features.includes(GITHUB_UAT_FEATURE));

/** Asked again whenever the computer says something on GitHub changed. */
function useGithubRead<T>(key: string | null, read: (session: HostSession) => Promise<T>, cache: WeakMap<HostSession, Map<string, T>>) {
  const { session, profile } = useApp();
  const link = useLink();
  const supported = useGithubSupported();
  const allowed = useHost((s) => s.grants?.tasks !== false);
  const changed = useHost((s) => s.githubRevision);
  const cached = () => (key ? cache.get(session)?.get(key) ?? null : null);
  const [value, setValue] = useState<T | null>(cached);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [revision, refresh] = useState(0);
  const reload = useCallback(() => refresh((n) => n + 1), []);
  useEffect(() => {
    let cancelled = false;
    setValue(cached());
    if (!key || !supported || !allowed || link.state !== "online") return;
    setLoading(true);
    void read(session).then((v) => {
      if (cancelled) return;
      let map = cache.get(session);
      if (!map) { map = new Map(); cache.set(session, map); }
      map.set(key, v); setValue(v); setError(null);
    }).catch((e: unknown) => { if (!cancelled) setError(describeError(e, profile.name)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
    // `read` is rebuilt each render; the key names what it reads.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session, profile.name, link.state, key, supported, allowed, changed, revision]);
  return { value, error, loading, supported, allowed, reload };
}

const projects = new WeakMap<HostSession, Map<string, GithubProjectView>>();
const lists = new WeakMap<HostSession, Map<string, ReadItems>>();
const prs = new WeakMap<HostSession, Map<string, GithubPrView>>();

/** A list as read, with when its read began. */
export type ReadItems = GithubItems & { readAt: number };

/** What the person just did to an issue or pull request: closed it, or moved it to a group. */
export type ItemChange = { closed: true } | { group: TriageGroup | null };

/** A change shown on the lists before GitHub is read again. */
export interface ExpectedItem {
  projectId: string;
  number: number;
  change: ItemChange;
  /** When the computer said it was done; null while it works on it. */
  done: number | null;
}

/** How long GitHub's list may still show what was just closed. */
export const CLOSE_LAG_MS = 10_000;
/** How long a done change is kept for a list read before it. */
const KEPT_MS = 10 * 60_000;

const expectedItems = new WeakMap<HostSession, Store<ExpectedItem[]>>();
const expectedStore = (session: HostSession) => {
  let store = expectedItems.get(session);
  if (!store) { store = new Store<ExpectedItem[]>([]); expectedItems.set(session, store); }
  return store;
};

/**
 * Shows `change` on the project's lists at once. `settled(true)` once the
 * computer has done it keeps it until GitHub is read again; `settled(false)`
 * takes it off, putting the item back as it was.
 */
export function expectItem(session: HostSession, projectId: string, number: number, change: ItemChange): (ok: boolean) => void {
  const store = expectedStore(session);
  const entry: ExpectedItem = { projectId, number, change, done: null };
  const now = Date.now();
  store.set((all) => [...all.filter((e) => !(e.projectId === projectId && e.number === number) && (e.done === null || now - e.done < KEPT_MS)), entry]);
  return (ok) => store.set((all) => {
    if (!all.includes(entry)) return all;
    return ok ? all.map((e) => (e === entry ? { ...entry, done: Date.now() } : e)) : all.filter((e) => e !== entry);
  });
}

const MOVED_NOTE: Record<TriageGroup, string> = { needs_you: "You asked it to wait for you.", agent_can_take: "You moved it here." };

/** Whether a change still stands over a list read at `readAt`. */
const stands = (e: ExpectedItem, readAt: number) =>
  e.done === null || readAt < e.done + ("closed" in e.change ? CLOSE_LAG_MS : 0);

/** The list with what the person has just done to it shown. */
export function withExpected(read: ReadItems, expected: ExpectedItem[]): GithubItems {
  const standing = expected.filter((e) => stands(e, read.readAt));
  if (!standing.length) return read;
  const items = read.items.flatMap((item) => {
    const e = standing.find((x) => x.number === item.number);
    if (!e) return [item];
    if ("closed" in e.change) return [];
    // As the computer records a move (`set_group` in github_run.rs).
    const { group: _, triage_note: __, ...rest } = item;
    const group = e.change.group;
    return [group ? { ...rest, group, triage_note: MOVED_NOTE[group], grouped_by_you: true } : { ...rest, grouped_by_you: false }];
  });
  return { ...read, items };
}

/** A project's repository and GitHub settings. */
export function useGithubProject(projectId: string | null) {
  const r = useGithubRead(projectId, async (s) => (await s.query({ kind: "github_project", project_id: projectId! })).view, projects);
  return { view: r.value, error: r.error, loading: r.loading, supported: r.supported, allowed: r.allowed, reload: r.reload };
}

/** A project's open issues or pull requests, with what the person just did to them. */
export function useGithubItems(projectId: string | null, list: GithubItemKind, filter = "") {
  const { session } = useApp();
  const key = projectId ? `${projectId}/${list}/${filter}` : null;
  const r = useGithubRead(key, async (s) => {
    const readAt = Date.now();
    return { ...(await s.query({ kind: "github_items", project_id: projectId!, list, ...(filter ? { filter } : {}) })).items, readAt };
  }, lists);
  const expected = useStore(expectedStore(session), useCallback((all: ExpectedItem[]) => all.filter((e) => e.projectId === projectId), [projectId]), shallowEqual);
  const items = useMemo(() => (r.value ? withExpected(r.value, expected) : null), [r.value, expected]);
  return { items, error: r.error, loading: r.loading, reload: r.reload };
}

/** One pull request in full. */
export function useGithubPr(projectId: string | null, number: number | null) {
  const key = projectId && number !== null ? `${projectId}/${number}` : null;
  const r = useGithubRead(key, async (s) => (await s.query({ kind: "github_pr", project_id: projectId!, number: number! })).pr, prs);
  return { pr: r.value, error: r.error, loading: r.loading, reload: r.reload };
}

/**
 * Sends one GitHub action, waits for the computer to carry it out and gives
 * what it made, if anything. Throws its answer, in words, if it refused or
 * the action failed.
 */
export function useGithubAct() {
  const { session, profile } = useApp();
  return useCallback(async (action: GithubAction, label: string): Promise<CommandResult | null> => {
    try {
      const receipt = await session.command({ command: "github", action }, { label });
      return await commandResult(session, receipt, { waitMs: ACT_WAIT_MS });
    } catch (e) { throw new Error(describeError(e, profile.name)); }
  }, [session, profile.name]);
}

/** How a list orders what the agent can take: what it works on first, what is done last. */
const STATUS_ORDER: Record<ItemStatus | "none", number> = { agent_working: 0, stopped: 1, waiting_for_you: 1, queued: 2, none: 3, green: 4 };

/**
 * Issues and PRs sorted for the project's list. Needs you holds what has a
 * question for the person or was put there; anything an agent is on goes
 * with what it can take, whatever the last sort said; the rest is not
 * sorted yet.
 */
export function groupItems(items: GithubItem[]): { take: GithubItem[]; needs: GithubItem[]; rest: GithubItem[] } {
  const needs = items.filter((i) => i.group === "needs_you" || i.status === "waiting_for_you");
  const take = items.filter((i) => !needs.includes(i) && (i.group === "agent_can_take" || !!i.status))
    .map((item, at) => ({ item, at }))
    .sort((a, b) => STATUS_ORDER[a.item.status ?? "none"] - STATUS_ORDER[b.item.status ?? "none"] || a.at - b.at)
    .map(({ item }) => item);
  return { take, needs, rest: items.filter((i) => !take.includes(i) && !needs.includes(i)) };
}

/**
 * The list as it stands now: each issue with the task working it and how
 * that stands, read from the tasks as they go, as the list itself is read
 * again only when GitHub changes. A task started on an issue since shows at
 * once, and an archived one no longer works it.
 */
export function useLiveItems(projectId: string, items: GithubItem[] | undefined): GithubItem[] | undefined {
  const linked = useHost((s) => {
    const tasks = projectTasks(s, projectId).filter((t) => t.github?.issue != null);
    return (items ?? []).map((i) => (i.pr ? undefined : linkedTask(tasks, i.number)));
  }, shallowEqual);
  const known = useHost((s) => (items ?? []).map((i) => !!i.task_id && !i.pr && !!s.tasks[i.task_id]), shallowEqual);
  return useMemo(() => {
    if (!items) return items;
    return items.map((i, at) => {
      const task = linked[at];
      // A task this device hasn't seen is left as the computer said.
      if (!task && !known[at]) return i;
      const { status: _, task_id: __, ...rest } = i;
      const status = task && itemStatus(task);
      return { ...rest, ...(task ? { task_id: task.task_id } : {}), ...(status ? { status } : {}) };
    });
  }, [items, linked, known]);
}

/** `#12`, an issue or PR URL in this repository, or a bare number. */
export function parseNumber(typed: string, repo?: { owner: string; name: string }): number | null {
  const t = typed.trim();
  // GitHub numbers its issues and pull requests from 1.
  const plain = /^#?([1-9]\d{0,8})$/.exec(t);
  if (plain) return Number(plain[1]);
  const url = /^https?:\/\/github\.com\/([^/\s]+)\/([^/\s]+)\/(?:issues|pull)\/([1-9]\d{0,8})(?:[/?#].*)?$/i.exec(t);
  if (!url) return null;
  if (repo && (url[1]!.toLowerCase() !== repo.owner.toLowerCase() || url[2]!.toLowerCase() !== repo.name.toLowerCase())) return null;
  return Number(url[3]);
}

/**
 * Starts the person's own word on how to open the app, after a review's
 * request. The computer finds it by this text (`DIRECTION` in
 * crates/services/src/app_start.rs), so change both together.
 */
export const OPEN_DIRECTION = "How to open it: ";

/** What a link asks for, as a new task's request before the person edits it. */
export function linkText(link: GithubLink, title?: string): string {
  const named = (what: string) => (title ? `${what}: ${title}` : what);
  switch (link.type) {
    case "issue": return named(`Fix #${link.number}`);
    case "pr": return named(`Take over pull request #${link.number}`);
    case "check": return `Check pull request #${link.number}`;
    case "stack": return "";
    case "new_pr": return "";
    case "find": {
      const where = link.scope.scope === "project" ? "the project" : link.scope.scope === "folder" ? link.scope.path
        : link.scope.scope === "since_last_run" ? "the commits since the last run" : `pull request #${link.scope.number}`;
      if (usesIt(link.focus)) return `Try ${link.scope.scope === "since_last_run" ? "what’s new in the app" : "the app"} as ${article(FOCUS_ISSUES[link.focus!])}`;
      return `Find ${link.focus ? `${FOCUS_ISSUES[link.focus]} issues` : "issues"} in ${where}`;
    }
    case "triage": return "Sort the open issues and pull requests";
  }
}

/**
 * Puts GitHub work in the project's new-task draft and opens Review, where
 * nothing starts until the person chooses. The draft starts fresh: words
 * the person wrote for another task are set aside in Saved drafts, not
 * reused, and an earlier GitHub draft left unsent gives way. Plan is
 * chosen for read-only work or a stranger's words; once queued, it starts
 * by itself.
 * Throws while the draft's last send is unaccounted for.
 */
export function draftGithubTask(ui: UiStore, projectId: string, d: { link: GithubLink; text?: string; agent?: AgentChoice | null; plan?: boolean }): { setAside: boolean } {
  return draftNewTask(ui, projectId, {
    link: d.link, mode: readOnlyLink(d.link) || d.plan ? "plan" : "work", text: d.text ?? linkText(d.link), agent: d.agent,
  });
}

/**
 * Puts a new task in the project's new-task draft and opens Review, where
 * nothing starts until the person chooses: GitHub work, or work an agent
 * proposed. Words the person wrote for another task are set aside in Saved
 * drafts, as with GitHub work.
 */
export function draftNewTask(ui: UiStore, projectId: string, d: { text: string; mode: TaskMode; title?: string; link?: GithubLink; agent?: AgentChoice | null; from?: TaskMode }): { setAside: boolean } {
  const prev = ui.draft(projectId, null);
  if (prev?.attempt) throw new Error("This project’s new-task draft is waiting to hear about its last send. Open it in Chat first.");
  // GitHub work can be drafted again from GitHub; the person's own words can't.
  const setAside = !!prev && !prev.review?.github && (!!prev.text.trim() || !!prev.annotationId);
  const key = draftKey(projectId, null);
  ui.set((s) => {
    const drafts = { ...s.drafts };
    delete drafts[key];
    return { ...s, drafts, savedDrafts: setAside ? { ...s.savedDrafts, [key]: [...(s.savedDrafts[key] ?? []), prev!] } : s.savedDrafts };
  });
  ui.saveDraft(projectId, null, {
    review: { ...freshReview(), title: d.title?.slice(0, 120) ?? "", github: d.link ?? null, ...(d.from ? { from: d.from } : {}) },
    mode: d.mode,
    text: d.text,
    agent: d.agent ?? prev?.agent ?? null,
  });
  if (!ui.persistNow()) throw new Error("The draft couldn’t be saved on this device.");
  ui.selectTask(projectId, null);
  ui.push({ screen: "review", projectId, taskId: null, intent: "new_task" }, "talk");
  return { setAside };
}
