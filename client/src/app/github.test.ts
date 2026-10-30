// SPDX-License-Identifier: Apache-2.0
import { beforeEach, expect, it } from "vitest";
import type { GithubItem, PrSummary } from "../protocol/types";
import { CLOSE_LAG_MS, draftGithubTask, type ExpectedItem, fixChoiceOf, fixSent, groupItems, lastRole, linkText, linkWords, parseNumber, prWords, readOnlyLink, rememberRole, withExpected } from "./github";
import { prefs } from "./prefs";
import { UiStore } from "./ui";

beforeEach(() => localStorage.clear());

const repo = { owner: "samrivera", name: "tidewater-api" };

it("reads a number, #number or this repository's link, and nothing else", () => {
  expect(parseNumber("219")).toBe(219);
  expect(parseNumber(" #219 ")).toBe(219);
  expect(parseNumber("https://github.com/samrivera/tidewater-api/issues/219", repo)).toBe(219);
  expect(parseNumber("https://github.com/SamRivera/Tidewater-API/pull/214/files#diff", repo)).toBe(214);
  expect(parseNumber("https://github.com/someone/else/pull/214", repo)).toBeNull();
  expect(parseNumber("https://github.com/someone/else/pull/214")).toBe(214);
  for (const bad of ["", "#", "0", "#0", "12a", "1234567890", "https://gitlab.com/samrivera/tidewater-api/issues/2", "https://github.com/samrivera/tidewater-api/commit/219"])
    expect(parseNumber(bad, repo), bad).toBeNull();
});

it("words each link as Review and a new task's request show it", () => {
  expect(linkText({ type: "issue", number: 219 }, "Retries double-charge")).toBe("Fix #219: Retries double-charge");
  expect(linkText({ type: "pr", number: 214 })).toBe("Take over pull request #214");
  expect(linkText({ type: "check", number: 214 })).toBe("Check pull request #214");
  expect(linkText({ type: "new_pr" })).toBe("");
  expect(linkText({ type: "triage" })).toBe("Sort the open issues and pull requests");
  expect(linkText({ type: "find", scope: { scope: "folder", path: "src/web" }, focus: "security", fix_with: { harness: "claude_code", model: null, effort: null } }))
    .toBe("Find security issues in src/web");
  expect(linkText({ type: "find", scope: { scope: "since_last_run" }, fix_with: { harness: "claude_code", model: null, effort: null } }))
    .toBe("Find issues in the commits since the last run");
  expect(linkWords(null)).toBe("Not linked");
  expect(linkWords({ type: "stack", number: 214 })).toBe("Stack on PR #214");
  expect(readOnlyLink({ type: "triage" })).toBe(true);
  expect(readOnlyLink({ type: "check", number: 1 })).toBe(true);
  expect(readOnlyLink({ type: "issue", number: 1 })).toBe(false);
  expect(readOnlyLink(null)).toBe(false);
});

it("puts a pull request's state, checks and threads in a few words", () => {
  const pr: PrSummary = { number: 214, url: "u", state: "open", draft: false, head: "h", passed: 3, failed: 0, pending: 0, unresolved: 1, conflict: false };
  expect(prWords(pr)).toEqual(["PR #214", "Ready", "Checks pass", "1 open thread"]);
  expect(prWords({ ...pr, draft: true, failed: 1, pending: 2, unresolved: 2, conflict: true })).toEqual(["PR #214", "Draft", "1 failing", "2 running", "2 open threads", "Conflict"]);
  // A closed or merged one says only that.
  expect(prWords({ ...pr, state: "merged", failed: 1 })).toEqual(["PR #214", "Merged"]);
  expect(prWords({ ...pr, state: "closed" })).toEqual(["PR #214", "Closed"]);
});

it("groups the list by triage; only a question or a hold needs you, and work in hand sits with what the agent can take", () => {
  const item = (number: number, more: Partial<GithubItem>): GithubItem => ({
    number, pr: false, title: `#${number}`, url: "u", author: "a", author_writes: true, mine: false, draft: false, labels: [], updated_at: 0, ...more,
  });
  const items = [
    item(1, { group: "agent_can_take" }), item(2, { group: "needs_you" }), item(3, { status: "waiting_for_you" }), item(4, {}),
    item(5, { group: "agent_can_take", status: "waiting_for_you" }), item(6, { status: "queued" }), item(7, { group: "agent_can_take", status: "agent_working" }),
    item(8, { group: "agent_can_take", status: "green" }),
  ];
  const g = groupItems(items);
  // Working first, then waiting in Next up, then the rest in the list's order.
  expect(g.take.map((i) => i.number)).toEqual([7, 6, 1, 8]);
  expect(g.needs.map((i) => i.number)).toEqual([2, 3, 5]);
  expect(g.rest.map((i) => i.number)).toEqual([4]);
});

it("shows a close or a move at once, until GitHub is read after the computer did it", () => {
  const item = (number: number, more: Partial<GithubItem> = {}): GithubItem => ({
    number, pr: false, title: `#${number}`, url: "u", author: "a", author_writes: true, mine: false, draft: false, labels: [], updated_at: 0, ...more,
  });
  const read = (readAt: number) => ({ readAt, truncated: false, items: [item(1), item(2, { group: "needs_you", triage_note: "The Sort asks" }), item(3, { group: "agent_can_take", grouped_by_you: true })] });
  const expected = (number: number, change: ExpectedItem["change"], done: number | null): ExpectedItem => ({ projectId: "p", number, change, done });
  const all = [expected(1, { closed: true }, null), expected(2, { group: "agent_can_take" }, null), expected(3, { group: null }, null)];
  const shown = withExpected(read(1000), all).items;
  // Closed is gone; a move takes the computer's own note for it.
  expect(shown.map((i) => i.number)).toEqual([2, 3]);
  expect(shown[0]).toMatchObject({ group: "agent_can_take", triage_note: "You moved it here.", grouped_by_you: true });
  expect(shown[1]!.group).toBeUndefined();
  expect(shown[1]!.grouped_by_you).toBe(false);
  // Done at 5000: a list read before then still shows it; one read after is GitHub's word.
  const done = all.map((e) => ({ ...e, done: 5000 }));
  expect(withExpected(read(4000), done).items.map((i) => i.group)).toEqual(["agent_can_take", undefined]);
  expect(withExpected(read(6000), done).items.map((i) => i.group)).toEqual(["needs_you", "agent_can_take"]);
  // Except a close: GitHub's list can still have it for a few seconds.
  expect(withExpected(read(6000), done).items.map((i) => i.number)).toEqual([2, 3]);
  expect(withExpected(read(5000 + CLOSE_LAG_MS), done).items.map((i) => i.number)).toEqual([1, 2, 3]);
  const fresh = read(0);
  expect(withExpected(fresh, [])).toBe(fresh);
});

it("remembers each project's finder and fixer on this phone", () => {
  const sol = { harness: "codex" as const, model: "sol-6.1", effort: "high" };
  expect(lastRole("p", "proj", "find")).toBeNull();
  rememberRole("p", "proj", "find", sol);
  rememberRole("p", "proj", "fix", { harness: "claude_code", model: "opus-5.5", effort: null });
  expect(lastRole("p", "proj", "find")).toEqual(sol);
  expect(lastRole("p", "proj", "fix")).toEqual({ harness: "claude_code", model: "opus-5.5", effort: null });
  expect(lastRole("p", "other", "find")).toBeNull();
  expect(lastRole("q", "proj", "find")).toBeNull();
  // The fixer's launch flags, once chosen, are kept with it.
  rememberRole("p", "proj", "fix", { harness: "claude_code", model: null, effort: null, launch_policy: "accept_edits" });
  expect(lastRole("p", "proj", "fix")).toEqual({ harness: "claude_code", model: null, effort: null, launch_policy: "accept_edits" });
  localStorage.setItem("github.roles.p", "not json");
  expect(lastRole("p", "proj", "find")).toBeNull();
});

it("sends a Find's fixes with their own launch flags, else this phone's defaults", () => {
  const opus = { harness: "claude_code" as const, model: "opus-5.5", effort: null };
  expect(fixSent(opus, true)).toEqual({ ...opus, launch_policy: "bypass_permissions" });
  expect(fixSent({ ...opus, launch_policy: "standard" }, true)).toEqual({ ...opus, launch_policy: "standard" });
  prefs.set(p => ({ ...p, claudeBypassPermissions: false }));
  expect(fixSent(opus, true)).toEqual({ ...opus, launch_policy: "standard" });
  // A computer that can't apply them is sent none.
  expect(fixSent({ ...opus, launch_policy: "bypass_permissions" }, false)).toEqual(opus);
});

it("keeps a fixer's launch flags where the sheet couldn't offer them, and adds none of this phone's", () => {
  const opus = { harness: "claude_code" as const, model: "opus-5.5", effort: null };
  const picked = { ...opus, launchPolicy: "bypass_permissions" as const };
  expect(fixChoiceOf(picked, true, opus)).toEqual({ ...opus, launch_policy: "bypass_permissions" });
  expect(fixChoiceOf(picked, false, opus)).toEqual(opus);
  expect(fixChoiceOf({ ...picked, model: "opus-5" }, false, { ...opus, launch_policy: "standard" })).toEqual({ ...opus, model: "opus-5", launch_policy: "standard" });
  // Another agent's flags don't carry over to it.
  const codex = { harness: "codex" as const, model: null, effort: null };
  expect(fixChoiceOf({ ...codex, launchPolicy: "sandboxed" }, false, { ...opus, launch_policy: "accept_edits" })).toEqual(codex);
});

it("drafts GitHub work fresh in Review, setting the person's words aside and refusing over an unanswered send", () => {
  const ui = new UiStore("github-draft");
  const astra = { harness: "codex" as const, model: "gpt-6-astra", effort: "high" };
  expect(draftGithubTask(ui, "proj", { link: { type: "check", number: 214 }, agent: astra })).toEqual({ setAside: false });
  let d = ui.draft("proj", null)!;
  expect(d).toMatchObject({ text: "Check pull request #214", mode: "plan", agent: astra, review: { authorization: "auto_start", github: { type: "check", number: 214 } } });
  expect(ui.get().tab).toBe("talk");
  expect(ui.get().stacks.talk.at(-1)).toMatchObject({ screen: "review", projectId: "proj", taskId: null, intent: "new_task" });

  // An unsent GitHub draft gives way: none of its words, branch or choices carry over.
  ui.saveDraft("proj", null, { text: "Check pull request #214 and mind the retry budget", review: { ...d.review!, title: "Old", branch: { name: "fix/x", new: true } } });
  expect(draftGithubTask(ui, "proj", { link: { type: "issue", number: 219 }, text: "Fix #219: Export drops rows" })).toEqual({ setAside: false });
  d = ui.draft("proj", null)!;
  expect(d).toMatchObject({ text: "Fix #219: Export drops rows", mode: "work", agent: astra, review: { title: "", github: { type: "issue", number: 219 } } });
  expect(d.review!.branch).toBeUndefined();
  expect(ui.get().savedDrafts).toEqual({});

  // Words the person wrote for a task of their own go to Saved drafts.
  ui.clearDraft("proj", null);
  ui.saveDraft("proj", null, { text: "Mind the retry budget" });
  expect(draftGithubTask(ui, "proj", { link: { type: "issue", number: 220 } })).toEqual({ setAside: true });
  expect(ui.draft("proj", null)).toMatchObject({ text: "Fix #220", review: { github: { type: "issue", number: 220 } } });
  expect(ui.get().savedDrafts["proj/new"]).toMatchObject([{ text: "Mind the retry budget" }]);

  ui.saveDraft("proj", null, { attempt: { phase: "unknown", at: 1, commandId: "c", detail: "d", text: "Fix #220" } });
  expect(() => draftGithubTask(ui, "proj", { link: { type: "triage" } })).toThrow(/waiting to hear about its last send/);
  expect(ui.draft("proj", null)!.review!.github).toEqual({ type: "issue", number: 220 });
});
