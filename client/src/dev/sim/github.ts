// SPDX-License-Identifier: Apache-2.0
import type {
  Command, CommandResult, GithubAction, GithubItem, GithubItemKind, GithubItems, GithubPrView, GithubProjectView, GithubRepo,
  GithubSettings, IssueRun, LaunchChoice, MergeTogether, ProjectFolder, ProtocolError, RunFix, RunStep, Task, TaskBranch, TaskGithub,
  TriageGroup,
} from "../../protocol/types";
import { going, itemStatus, writesNothing } from "../../state/githubStatus";
import type { SimHost } from "./host";
import { ids } from "./scenario";
import { commits as reviewCommits } from "./reviews";

const fail = (code: ProtocolError["code"], message: string): ProtocolError => ({ code, message } as ProtocolError);
const MIN = 60_000;
/** How long the sim's merge check takes to test what was picked. */
const TOGETHER_MS = 6_000;
/** `crates/services/src/github_run.rs`. */
const CARRY_ON = "Carry on where you stopped.";
const PAUSED = "Run all is paused. Resume it to carry on.";
/** `NOT_A_REPO` and its wording in `github_project`. */
const NOT_A_REPO = "This project's folder isn't a Git repository, so GitHub can't be used with it. GitHub works in a project whose folder is one, with its `origin` on github.com.";
const notARepo = (folders: ProjectFolder[]): string => {
  const head = "This project's folder isn't a Git repository, so GitHub can't be used with it.";
  const names = folders.map((f) => f.name).join(", ");
  return !folders.length ? NOT_A_REPO
    : folders.length === 1 ? `${head} The folder ${names} inside it is one, on GitHub: use GitHub in a project of that folder instead.`
      : `${head} The folders ${names} inside it are, on GitHub: use GitHub in a project of one of those instead.`;
};

/** A project's Run all as kept: what the phone sees, and the fixes Pause stopped. */
interface Running { view: IssueRun; pausedTasks: string[] }

/** `GithubSettings::default()` in `crates/protocol/src/github.rs`. */
export const defaultSettings = (): GithubSettings => ({
  mode: "off", warning_answered: false, branch_prefix: "ophio/", label: true,
  workflow: { fix_findings: "by_itself", check_before_merge: true, findings_per_run: 5 },
});

/** `slug` in `crates/services/src/github.rs`. */
function slug(title: string): string {
  let s = "";
  for (const c of title.toLowerCase()) {
    if (/[a-z0-9]/.test(c)) s += c;
    else if (s && !s.endsWith("-")) s += "-";
    if (s.length >= 40) break;
  }
  return s.replace(/-+$/, "");
}

/** What a linked task starts with (`prepare_link`). */
export interface PreparedLink { github: TaskGithub; title: string | null; branch: TaskBranch | null; plan: boolean }

/**
 * The computer's GitHub workflow (`crates/services/src/github.rs`) against
 * one repository: tidewater-api is on GitHub, field-notes' `origin` isn't,
 * and garden-planner's folder isn't a repository but its `app` is.
 */
export class GithubSim {
  readonly settings = new Map<string, GithubSettings>();
  login = "samrivera";
  repo: GithubRepo = {
    owner: "samrivera", name: "tidewater-api", url: "https://github.com/samrivera/tidewater-api", default_branch: "main",
    private: true, admin: true, push: true, owned: true, protected: false, runs_checks: true, deploys: false, private_reporting: false,
  };
  readonly items: GithubItem[];
  readonly prs = new Map<number, GithubPrView>();
  /** Each project's latest Run all. */
  readonly runs = new Map<string, Running>();
  /** The repositories on GitHub directly in a project's folder that isn't one. */
  readonly repoFolders = new Map<string, string[]>([["/home/sam/Projects/garden-planner", ["app"]]]);
  /** Each project's Merge together, until the person clears it. */
  readonly together = new Map<string, MergeTogether>();
  /** Items the person put in a group themselves; a Sort leaves them there. */
  readonly #byYou = new Set<string>();
  #timers: ReturnType<typeof setTimeout>[] = [];
  constructor(private readonly host: SimHost, now: number) {
    this.settings.set(ids.pTide, { ...defaultSettings(), mode: "drafts" });
    const url = (n: number, pr: boolean) => `${this.repo.url}/${pr ? "pull" : "issues"}/${n}`;
    const item = (number: number, pr: boolean, title: string, author: string, rest: Partial<GithubItem> = {}): GithubItem => ({
      number, pr, title, url: url(number, pr), author, author_writes: true, mine: author === this.login, draft: false, labels: [],
      updated_at: now - number * 7 * MIN, ...rest,
    });
    this.items = [
      item(219, false, "Retry flake in the orders webhook test", "samrivera", { labels: ["bug"], group: "agent_can_take", triage_note: "CI logs show the race; one test file." }),
      item(221, false, "Orders export times out for large accounts", "jpark", { group: "needs_you", triage_note: "Should big exports page through the orders, which is simpler but slower, or stream them as they are read? I’d stream: the largest accounts time out either way today.", triage_pick: "Stream them" }),
      item(223, false, "Rate limits page is out of date", "drive-by-dev", { author_writes: false, labels: ["docs"] }),
      item(214, true, "Add rate-limit headers to /v1/orders", "samrivera", { labels: ["agent"], task_id: ids.tHeaders, status: "green", checks: "passed", merge: "ready" }),
      item(217, true, "Page the orders export", "jpark", { draft: true, group: "needs_you", triage_note: "Draft; waiting on #221’s decision.", checks: "failed", merge: "blocked" }),
      item(218, true, "Sign update bundles before they're published", "samrivera", { labels: ["agent"], task_id: ids.tSigning, status: "green", checks: "passed", merge: "ready" }),
      item(220, true, "Stream the orders export", "samrivera", { labels: ["agent"], task_id: ids.tExport, checks: "failed", merge: "blocked" }),
    ];
    this.prs.set(214, {
      repo: `${this.repo.owner}/${this.repo.name}`,
      summary: { number: 214, url: url(214, true), state: "open", draft: false, head: "5b7e2c9d41aa0f3e8b1c2d4e5f60718293a4b5c6", passed: 3, failed: 0, pending: 0, unresolved: 1, conflict: false },
      title: "Add rate-limit headers to /v1/orders",
      body: "Adds `X-RateLimit-Limit`, `X-RateLimit-Remaining` and `X-RateLimit-Reset` to every `/v1/orders` response.\n\n- Shares the limiter's counters, so the numbers match what it enforces\n- Tests cover the reset after a window ends\n\n🤖 Made by GPT-6-Astra via Codex in Øphio, working for @samrivera",
      author: "samrivera", head_branch: "ophio/rate-limit-headers", base_branch: "main",
      checks: [
        { name: "build", state: "passed", required: true, url: `${this.repo.url}/actions/runs/1` },
        { name: "test", state: "passed", required: true, url: `${this.repo.url}/actions/runs/2` },
        { name: "lint", state: "passed", required: false },
        { name: "preview", state: "other", required: false },
      ],
      threads: [{
        id: "PRRT_1", resolved: false, path: "src/orders/limits.rs", line: 42, author: "jpark", url: `${url(214, true)}#discussion_r1`,
        body: "Should Reset be seconds from now or a timestamp? The other endpoints use seconds.",
      }],
      mergeable: true, method: "squash", task_id: ids.tHeaders, roles: ["Made by GPT-6-Astra"],
    });
    this.prs.set(218, {
      repo: `${this.repo.owner}/${this.repo.name}`,
      summary: { number: 218, url: url(218, true), state: "open", draft: false, head: reviewCommits.signing, passed: 2, failed: 0, pending: 0, unresolved: 0, conflict: false },
      title: "Sign update bundles before they're published",
      body: "Signs each update bundle with the release key before upload, and refuses to publish an unsigned one.\n\nAlso adds a line to REVIEW.md about release scripts.",
      author: "samrivera", head_branch: "ophio/sign-update-bundles", base_branch: "main",
      checks: [
        { name: "build", state: "passed", required: true, url: `${this.repo.url}/actions/runs/5` },
        { name: "test", state: "passed", required: true, url: `${this.repo.url}/actions/runs/6` },
      ],
      threads: [], mergeable: true, method: "squash", task_id: ids.tSigning, roles: ["Made by Claude Opus 5.5"],
    });
    this.prs.set(220, {
      repo: `${this.repo.owner}/${this.repo.name}`,
      summary: { number: 220, url: url(220, true), state: "open", draft: false, head: reviewCommits.exportNow, passed: 1, failed: 1, pending: 0, unresolved: 0, conflict: false },
      title: "Stream the orders export",
      body: "Streams the export as orders are read, so large accounts don't time out.",
      author: "samrivera", head_branch: "ophio/stream-orders-export", base_branch: "main",
      checks: [
        { name: "build", state: "passed", required: true, url: `${this.repo.url}/actions/runs/7` },
        { name: "test", state: "failed", required: true, url: `${this.repo.url}/actions/runs/8` },
      ],
      threads: [], mergeable: false, blocked: "The required check test is failing.", method: "squash", task_id: ids.tExport, roles: ["Made by GPT-6-Astra"],
    });
    this.prs.set(217, {
      repo: `${this.repo.owner}/${this.repo.name}`,
      summary: { number: 217, url: url(217, true), state: "open", draft: true, head: "9a1c4e7b20d35f6a8b9c0d1e2f3a4b5c6d7e8f90", passed: 1, failed: 1, pending: 0, unresolved: 0, conflict: false },
      title: "Page the orders export",
      body: "Pages the export by 5,000 orders. Waiting on #221 before it's ready.",
      author: "jpark", head_branch: "export-paging", base_branch: "main",
      checks: [
        { name: "build", state: "passed", required: true, url: `${this.repo.url}/actions/runs/3` },
        { name: "test", state: "failed", required: true, url: `${this.repo.url}/actions/runs/4` },
      ],
      threads: [],
      mergeable: false, blocked: "The required check test is failing.", method: "squash", roles: [],
    });
  }

  #settings(projectId: string): GithubSettings {
    return structuredClone(this.settings.get(projectId) ?? defaultSettings());
  }
  #onGithub(projectId: string): boolean {
    return projectId === ids.pTide;
  }
  #changed(projectId: string): void {
    this.host.emit({ type: "github_changed", project_id: projectId }, { kind: "companion" }, "observed");
  }

  project(projectId: string): GithubProjectView {
    const view: GithubProjectView = {
      project_id: projectId, login: this.login, settings: this.#settings(projectId), warnings: [],
      open_agent_prs: new Set(this.host.tasks.filter((t) => t.project_id === projectId && t.github?.pr?.state === "open").map((t) => t.github!.pr!.number)).size,
    };
    const project = this.host.projects.find((p) => p.project_id === projectId);
    // Signing in wouldn't help a folder that isn't a repository.
    const folders = project && project.vcs?.kind !== "git" ? this.#repoFolders(project.host_path) : null;
    const notRepo = folders ? { problem: notARepo(folders), ...(folders.length ? { repo_folders: folders } : {}) } : {};
    if (!this.login) {
      const { login: _, ...signedOut } = view;
      return { ...signedOut, problem: "Sign in to GitHub under Accounts first.", ...notRepo };
    }
    if (folders) return { ...view, ...notRepo };
    if (!this.#onGithub(projectId)) return { ...view, problem: "This project's `origin` isn't on github.com." };
    const warnings = [`Nothing protects \`${this.repo.default_branch}\`, so a mistake can land there without a pull request.`];
    const run = this.runs.get(projectId);
    const together = this.together.get(projectId);
    return {
      ...view, repo: structuredClone(this.repo), warnings, ...(run ? { run: structuredClone(run.view) } : {}),
      ...(together ? { together: structuredClone(together) } : {}),
    };
  }

  /** Whether a folder is one of those repositories, so a project of it is a Git one. */
  isRepoFolder(path: string): boolean {
    return [...this.repoFolders].some(([dir, names]) => names.some((n) => `${dir}/${n}` === path));
  }
  #repoFolders(dir: string): ProjectFolder[] {
    return (this.repoFolders.get(dir) ?? []).map((name) => {
      const path = `${dir}/${name}`;
      return { name, path, project_id: this.host.projects.find((p) => p.host_path === path)?.project_id ?? null, unavailable: null };
    });
  }

  list(projectId: string, list: GithubItemKind, filter?: string): GithubItems {
    if (!this.#onGithub(projectId)) throw fail("not_found", "This project's `origin` isn't on github.com.");
    const words = filter?.trim().toLowerCase() ?? "";
    const items = this.items.filter((i) => i.pr === (list === "prs") && (!words || i.title.toLowerCase().includes(words)))
      .map((i) => this.#withTask(projectId, i))
      .map((i) => { const head = i.pr ? this.prs.get(i.number)?.summary.head : undefined; return head ? { ...i, head } : i; });
    return { items: structuredClone(items), truncated: false };
  }

  /** `linked_task`: the issue's latest task that isn't archived, one that writes first. */
  #linked(projectId: string, number: number): Task | undefined {
    return this.host.tasks
      .filter((t) => t.project_id === projectId && t.state !== "archived" && t.github?.link.type === "issue" && t.github.link.number === number)
      .sort((a, b) => Number(writesNothing(a)) - Number(writesNothing(b)) || b.created_at - a.created_at)[0];
  }

  /** An issue with the task working it and how that stands, as `list_items` gives it. */
  #withTask(projectId: string, listed: GithubItem): GithubItem {
    const item = this.#byYou.has(`${listed.pr}:${listed.number}`) ? { ...listed, grouped_by_you: true } : listed;
    if (item.pr) return item;
    const task = this.#linked(projectId, item.number);
    if (!task) return item;
    const status = itemStatus(task);
    return { ...item, task_id: task.task_id, ...(status ? { status } : {}) };
  }

  /** `close_issue`: closed in the person's name; a fix waiting for it leaves Next up, and one that couldn't reproduce it asks nothing more. */
  #closeIssue(projectId: string, repo: string, number: number): void {
    if (!this.#onGithub(projectId)) throw fail("not_found", "This project's `origin` isn't on github.com.");
    const named = `${this.repo.owner}/${this.repo.name}`;
    if (repo.toLowerCase() !== named.toLowerCase())
      throw fail("conflict", `This project's \`origin\` is ${named} now, not the repository whose issue you read. Open it again.`);
    if (this.items.some((i) => i.pr && i.number === number)) throw fail("invalid_request", `#${number} is a pull request, not an issue`);
    const i = this.items.findIndex((x) => !x.pr && x.number === number);
    if (i >= 0) this.items.splice(i, 1);
    this.#byYou.delete(`false:${number}`);
    const task = this.#linked(projectId, number);
    if (!task) return;
    if (itemStatus(task) === "queued") {
      this.#takeOut(projectId, task, "Taken out of Next up: you closed its issue");
    } else if (task.github?.stage === "not_reproduced") {
      this.host.touchTask(task.task_id, { github: { ...task.github, stage: "closed" } });
      this.host.resolveTaskAttention(task.task_id);
    }
  }

  pr(projectId: string, number: number): GithubPrView {
    const pr = this.#onGithub(projectId) ? this.prs.get(number) : undefined;
    if (!pr) throw fail("not_found", `Pull request #${number} isn't on GitHub`);
    return structuredClone(pr);
  }

  act(action: GithubAction): CommandResult | null | Promise<CommandResult | null> {
    const id = action.project_id;
    switch (action.action) {
      case "run_issues":
        if (!this.#onGithub(id)) throw fail("not_found", "This project's `origin` isn't on github.com.");
        return this.#run(id, action.step).finally(() => this.#changed(id)).then(() => null);
      case "set_group":
        this.#setGroup(id, action.number, action.pr, action.group ?? null);
        break;
      case "close_issue":
        this.#closeIssue(id, action.repo, action.number);
        break;
      case "file_issue": {
        // `checked_title` and `checked_body` in `crates/services/src/github_tools.rs`.
        const title = action.title.trim();
        if (!title || [...title].length > 256 || title.includes("\n")) throw fail("invalid_request", "Give a title of one line, at most 256 characters.");
        const body = action.body.trim().replaceAll("\r", "");
        if ([...body].length > 60_000) throw fail("invalid_request", "That's longer than GitHub takes. Keep it under 60000 characters.");
        if (!this.#onGithub(id)) throw fail("not_found", "This project's `origin` isn't on github.com.");
        const named = `${this.repo.owner}/${this.repo.name}`;
        if (action.repo.toLowerCase() !== named.toLowerCase())
          throw fail("conflict", `This project's \`origin\` is ${named} now, not the repository you were filing in. Look again before filing.`);
        const number = Math.max(0, ...this.items.map((i) => i.number)) + 1;
        const url = `${this.repo.url}/issues/${number}`;
        this.items.unshift({
          number, pr: false, title, url, author: this.login, author_writes: true, mine: true, draft: false, labels: [], updated_at: Date.now(),
        });
        this.#changed(id);
        return { kind: "issue_filed", number, url };
      }
      case "set_settings": {
        const s = action.settings;
        if (s.branch_prefix.length > 40 || /[\s~^:?*[\\]|\.\.|^[-/.]|\/\/|@\{/.test(s.branch_prefix))
          throw fail("invalid_request", "That branch prefix isn't one Git accepts at the start of a branch name");
        if (s.workflow.findings_per_run < 1 || s.workflow.findings_per_run > 20) throw fail("invalid_request", "A finder run files 1 to 20 issues");
        if (s.mode !== "off" && !this.#onGithub(id)) throw fail("not_found", "This project's `origin` isn't on github.com.");
        const current = this.#settings(id);
        const warning_answered = s.mode === "through_merge" && current.mode !== "through_merge" ? false : current.warning_answered;
        this.settings.set(id, { ...structuredClone(s), warning_answered });
        break;
      }
      case "answer_warning": {
        const s = this.#settings(id);
        if (s.mode !== "through_merge") throw fail("conflict", "Through merge isn't chosen for this project any more");
        const named = `${this.repo.owner}/${this.repo.name}`;
        if (action.keep_through_merge && action.repo.toLowerCase() !== named.toLowerCase())
          throw fail("conflict", `This project's \`origin\` is ${named} now, not the repository whose warning was answered. Read its warning again.`);
        this.settings.set(id, { ...s, mode: action.keep_through_merge ? s.mode : "drafts", warning_answered: true });
        break;
      }
      case "protect_branch":
        if (!this.#onGithub(id)) throw fail("not_found", "This project's `origin` isn't on github.com.");
        this.repo.protected = true;
        break;
      case "mark_pr_ready": {
        const pr = this.#open(id, action.repo, action.number);
        pr.summary.draft = false;
        this.#record(action.number, (g) => { if (g.pr) g.pr.draft = false; });
        break;
      }
      case "merge_together":
        this.#mergeTogether(id, action);
        break;
      case "stop_together": {
        const t = this.together.get(id);
        if (t && t.ended_at == null) {
          for (const p of t.prs) if (p.state === "waiting" || p.state === "testing") Object.assign(p, { state: "left_out", why: "You stopped it." });
          delete t.check;
          Object.assign(t, { ended_at: Date.now(), stopped: true });
        } else this.together.delete(id);
        break;
      }
      case "merge_pr": {
        const pr = this.#open(id, action.repo, action.number);
        if (pr.summary.head !== action.expected_head)
          throw fail("conflict", `Pull request #${action.number} has a new commit, ${pr.summary.head.slice(0, 7)}, since you looked. Check it again before merging.`);
        if (pr.summary.draft) throw fail("conflict", `Pull request #${action.number} is a draft. Mark it ready first.`);
        if (!pr.mergeable) throw fail("conflict", pr.blocked ?? "GitHub won't merge it yet");
        pr.summary.state = "merged";
        this.#record(action.number, (g) => { if (g.pr) g.pr.state = "merged"; g.stage = "merged"; });
        const i = this.items.findIndex((x) => x.pr && x.number === action.number);
        if (i >= 0) this.items.splice(i, 1);
        break;
      }
    }
    this.#changed(id);
    return null;
  }

  dispose(): void {
    for (const t of this.#timers) clearTimeout(t);
  }

  /**
   * `merge_together`: each as the person saw it, tested together on a merge
   * check; one whose checks fail is left out and the rest merge in turn.
   */
  #mergeTogether(projectId: string, action: Extract<GithubAction, { action: "merge_together" }>): void {
    const was = this.together.get(projectId);
    if (was && was.ended_at == null)
      throw fail("conflict", "Merge together is already on in this project. Stop it, or wait for it to finish.");
    const chosen = [...action.prs].sort((a, b) => a.number - b.number);
    const prs = chosen.map((c) => {
      const pr = this.#open(projectId, action.repo, c.number);
      if (pr.summary.head !== c.head)
        throw fail("conflict", `Pull request #${c.number} has a new commit, ${pr.summary.head.slice(0, 7)}, since you looked. Check it again before merging.`);
      return pr;
    });
    const check = 230;
    const together: MergeTogether = {
      prs: prs.map((pr) => ({ number: pr.summary.number, title: pr.title, state: "testing" })), check, started_at: Date.now(), stopped: false,
    };
    this.together.set(projectId, together);
    this.#timers.push(setTimeout(() => {
      if (this.together.get(projectId) !== together || together.ended_at != null) return;
      for (const [i, p] of together.prs.entries()) {
        const pr = prs[i]!;
        const failed = pr.checks.filter((c) => c.state === "failed").map((c) => `\`${c.name}\``);
        if (failed.length) {
          Object.assign(p, { state: "left_out", why: `Tested with \`${this.repo.default_branch}\` on Øphio's merge check #${check}, this failed: ${failed.join(", ")}.` });
          continue;
        }
        p.state = "merged";
        pr.summary.state = "merged";
        this.#record(p.number, (g) => { if (g.pr) g.pr.state = "merged"; g.stage = "merged"; });
        const at = this.items.findIndex((x) => x.pr && x.number === p.number);
        if (at >= 0) this.items.splice(at, 1);
      }
      delete together.check;
      together.ended_at = Date.now();
      this.#changed(projectId);
    }, TOGETHER_MS));
  }

  #active(projectId: string): Running | undefined {
    const run = this.runs.get(projectId);
    return run && run.view.ended_at == null ? run : undefined;
  }
  #task(id: string): Task | undefined {
    return this.host.tasks.find((t) => t.task_id === id);
  }

  async #run(projectId: string, step: RunStep): Promise<void> {
    if (step === "start") return this.#start(projectId);
    const run = this.#active(projectId);
    if (!run) return;
    const working = () => run.view.fixes.map((f) => this.#task(f.task_id)).filter((t): t is Task => !!t && ["dispatching", "working"].includes(t.state));
    switch (step) {
      case "pause": {
        // Next up holds first, so the fix stopping isn't followed by the next.
        this.#holdQueue(projectId, PAUSED);
        run.view.paused = true;
        run.pausedTasks = working().map((t) => t.task_id);
        await this.#stopEach(run.pausedTasks);
        return;
      }
      case "resume": {
        for (const id of run.pausedTasks) {
          const t = this.#task(id);
          if (t?.state === "interrupted") await this.host.inside({ command: "resume_task", task_id: id, text: CARRY_ON }, t.revision);
        }
        run.view.paused = false;
        run.pausedTasks = [];
        this.#holdQueue(projectId, null);
        this.host.talk.advance(projectId);
        return;
      }
      case "stop": {
        Object.assign(run.view, { ended_at: Date.now(), stopped: true, paused: false });
        delete run.view.sorting;
        run.pausedTasks = [];
        const queue = this.host.talk.queue(projectId);
        const out = run.view.fixes.map((f) => this.#task(f.task_id))
          .filter((t): t is Task => t?.state === "accepted" && !t.current_run && queue.items.some((i) => i.task_id === t.task_id && i.state.state !== "started"));
        if (out.length) {
          queue.items = queue.items.filter((i) => i.state.state === "started" || !out.some((t) => t.task_id === i.task_id));
          queue.items.forEach((i, position) => { i.position = position; });
          this.host.talk.bump(queue); this.host.talk.refresh(queue); this.host.talk.put(queue);
          for (const t of out) this.host.touchTask(t.task_id, { state: "archived", status_line: "Taken out of Next up when Run all stopped" });
        }
        await this.#stopEach(working().map((t) => t.task_id));
        return;
      }
    }
  }

  async #start(projectId: string): Promise<void> {
    const s = this.#settings(projectId);
    if (s.mode === "off") throw fail("invalid_request", "Turn GitHub on for this project first");
    const fixWith = s.workflow.fix_with;
    if (!fixWith) throw fail("invalid_request", "Choose who fixes the issues first");
    let run = this.#active(projectId);
    if (!run) {
      run = { view: { started_at: Date.now(), fixes: [], paused: false, stopped: false }, pausedTasks: [] };
      this.runs.set(projectId, run);
    }
    const { added, sorting } = await this.#add(projectId, s, fixWith, true);
    for (const fix of added) if (!run.view.fixes.some((f) => f.task_id === fix.task_id)) run.view.fixes.push(fix);
    if (sorting) run.view.sorting = sorting;
    run.view.paused = false;
    run.pausedTasks = [];
    if (!run.view.fixes.length && !run.view.sorting) {
      run.view.ended_at = Date.now();
      throw fail("invalid_request", "Nothing to fix: no open issue is in Agent can take it, and none is waiting to be sorted");
    }
    // Run all is the go-ahead for Next up as well.
    this.#holdQueue(projectId, null);
    this.host.talk.advance(projectId);
  }

  /** `add_to_run`: a fix queued for each issue the agent can take that has none, each one waiting taken on, and a Sort. */
  async #add(projectId: string, s: GithubSettings, fixWith: LaunchChoice, sort: boolean): Promise<{ added: RunFix[]; sorting: string | null }> {
    const queue = this.host.talk.queue(projectId);
    const waiting = (id: string) => queue.items.some((i) => i.task_id === id && i.state.state !== "started");
    const added: RunFix[] = [];
    let failed: unknown = null;
    const create = (with_: LaunchChoice, prompt: string, github: Extract<Command, { command: "create_task" }>["github"], placement: Extract<Command, { command: "create_task" }>["placement"], mode: "plan" | null) =>
      this.host.inside({
        command: "create_task", agent_chosen: true, project_id: projectId, kind: "coding", ...(mode ? { mode } : {}), title: null, prompt, bundle: null,
        harness: with_.harness, model: with_.model, effort: with_.effort, ...(with_.launch_policy ? { launch_policy: with_.launch_policy } : {}),
        github, mission: null, target_id: null, placement,
      });
    for (const item of this.items.filter((i) => !i.pr)) {
      const t = this.#linked(projectId, item.number);
      const fix = t && !writesNothing(t) ? t : undefined;
      if (fix) {
        if (fix.state === "accepted" && waiting(fix.task_id)) added.push({ number: item.number, task_id: fix.task_id });
        continue;
      }
      if (item.group !== "agent_can_take") continue;
      try {
        const made = await create(fixWith, `Fix issue #${item.number}.`,
          { type: "issue", number: item.number, ...(s.workflow.review_with ? { review_with: s.workflow.review_with } : {}) },
          { mode: "queue", position: null, authorization: "auto_start" }, null);
        if (made?.kind === "task_created") added.push({ number: item.number, task_id: made.task_id });
      } catch (e) { failed ??= e; }
    }
    const unsorted = this.items.some((i) => !i.pr && !i.group && !this.#linked(projectId, i.number));
    let sorting = this.host.tasks.find((t) => t.project_id === projectId && t.github?.link.type === "triage" && going(t))?.task_id ?? null;
    if (!sorting && unsorted && sort) {
      const { launch_policy: _, ...sortWith } = s.workflow.review_with ?? fixWith;
      try {
        const made = await create(sortWith, "Sort the open issues and pull requests", { type: "triage" }, { mode: "start_now" }, "plan");
        if (made?.kind === "task_created") sorting = made.task_id;
      } catch (e) { failed ??= e; }
    }
    if (failed && !added.length && !sorting) throw failed;
    return { added, sorting };
  }

  /** A run ended in the project: a Run all fix moves it on however it ended, and its Sort sorts. */
  async ended(task: Task): Promise<void> {
    const projectId = task.project_id;
    const run = this.#active(projectId);
    if (task.github?.link.type === "triage" && this.#onGithub(projectId)) {
      // What the sim's Sort finds: those who can write to the repository wrote what the agent can take.
      for (const item of this.items) {
        if (item.group || this.#byYou.has(`${item.pr}:${item.number}`) || (!item.pr && this.#linked(projectId, item.number))) continue;
        item.group = item.author_writes && !item.pr ? "agent_can_take" : "needs_you";
        item.triage_note = item.group === "agent_can_take" ? "Small and clear; the issue says what to change." : "Read it first: it’s from someone who can’t write to the repository.";
      }
      if (run) {
        const s = this.#settings(projectId);
        if (s.workflow.fix_with && s.mode !== "off" && !run.view.paused) {
          const { added } = await this.#add(projectId, s, s.workflow.fix_with, false).catch(() => ({ added: [] as RunFix[] }));
          for (const fix of added) if (!run.view.fixes.some((f) => f.task_id === fix.task_id)) run.view.fixes.push(fix);
        }
        if (run.view.sorting === task.task_id) delete run.view.sorting;
      }
    } else if (!run?.view.fixes.some((f) => f.task_id === task.task_id)) return;
    if (run) {
      this.host.talk.advance(projectId);
      this.#settle(projectId);
    }
    this.#changed(projectId);
  }

  /** `settle`: Run all ends once nothing of it is waiting, working or being sorted. */
  #settle(projectId: string): void {
    const run = this.#active(projectId);
    if (!run || run.view.paused || this.host.talk.queue(projectId).paused) return;
    if (run.view.fixes.some((f) => going(this.#task(f.task_id))) || (run.view.sorting && going(this.#task(run.view.sorting)))) return;
    run.view.ended_at = Date.now();
    delete run.view.sorting;
  }

  /** The fixes of the project's Run all, whose go-ahead Next up keeps (`keep_running`). */
  running(projectId: string): Set<string> {
    return new Set(this.#active(projectId)?.view.fixes.map((f) => f.task_id));
  }

  #holdQueue(projectId: string, why: string | null): void {
    const talk = this.host.talk;
    const q = talk.queue(projectId);
    if (q.paused === !!why && (q.pause_reason ?? null) === why) return;
    talk.bump(q);
    q.paused = !!why;
    if (why) q.pause_reason = why; else delete q.pause_reason;
    talk.refresh(q); talk.put(q);
  }

  async #stopEach(tasks: string[]): Promise<void> {
    for (const task_id of tasks) await this.host.inside({ command: "stop_task", task_id }).catch(() => undefined);
  }

  /** `set_group`: where the person puts an item stands over a Sort; Needs you takes a waiting fix out. */
  #setGroup(projectId: string, number: number, pr: boolean, group: TriageGroup | null): void {
    if (!this.#onGithub(projectId)) throw fail("not_found", "This project's `origin` isn't on github.com.");
    const item = this.items.find((i) => i.pr === pr && i.number === number);
    if (!item) throw fail("not_found", `#${number} isn't open on GitHub`);
    if (group) {
      item.group = group;
      item.triage_note = group === "needs_you" ? "You asked it to wait for you." : "You moved it here.";
      delete item.triage_pick;
      this.#byYou.add(`${pr}:${number}`);
    } else {
      delete item.group; delete item.triage_note; delete item.triage_pick;
      this.#byYou.delete(`${pr}:${number}`);
    }
    if (group !== "needs_you" || pr) return;
    const task = this.#linked(projectId, number);
    if (!task || itemStatus(task) !== "queued") return;
    this.#takeOut(projectId, task, "Taken out of Next up: it waits for you");
  }

  /** Takes a fix waiting in Next up out of it and Run all: `take_out_fix`. */
  #takeOut(projectId: string, task: Task, why: string): void {
    const run = this.#active(projectId);
    if (run) run.view.fixes = run.view.fixes.filter((f) => f.task_id !== task.task_id);
    const queue = this.host.talk.queue(projectId);
    if (!queue.items.some((i) => i.task_id === task.task_id && i.state.state !== "started")) return;
    queue.items = queue.items.filter((i) => i.task_id !== task.task_id || i.state.state === "started");
    queue.items.forEach((i, position) => { i.position = position; });
    this.host.talk.bump(queue); this.host.talk.refresh(queue); this.host.talk.put(queue);
    this.host.touchTask(task.task_id, { state: "archived", status_line: why });
  }

  #open(projectId: string, repo: string, number: number): GithubPrView {
    const pr = this.prs.get(number);
    if (!this.#onGithub(projectId) || !pr) throw fail("not_found", `Pull request #${number} isn't on GitHub`);
    const named = `${this.repo.owner}/${this.repo.name}`;
    if (repo.toLowerCase() !== named.toLowerCase())
      throw fail("conflict", `This project's \`origin\` is ${named} now, not the repository whose pull request you read. Read it again.`);
    if (pr.summary.state !== "open") throw fail("conflict", `Pull request #${number} isn't open`);
    return pr;
  }

  /** Writes a pull request's change onto the tasks linked to it. */
  #record(number: number, change: (g: TaskGithub) => void): void {
    for (const t of this.host.tasks) {
      if (t.github?.pr?.number !== number) continue;
      const github = structuredClone(t.github);
      change(github);
      this.host.touchTask(t.task_id, { github });
    }
    this.host.emit({ type: "pr_changed", project_id: ids.pTide, number }, { kind: "companion" }, "observed");
  }

  /** `prepare_link`: what a linked task starts with, read from GitHub. */
  prepare(c: Extract<Command, { command: "create_task" }>): PreparedLink | null {
    const link = c.github;
    if (!link) return null;
    if (c.place) throw fail("invalid_request", "A task on its own isn't in the project's repository, so it can't be linked to GitHub");
    // Mirrors prepare_link: a finder that uses the app does it on a screen.
    const usesIt = link.type === "find" && ["everyday_user", "novice_user", "power_user"].includes(link.focus ?? "");
    if (usesIt && c.kind !== "computer_use") throw fail("invalid_request", "A reviewer that uses the app works on a screen, so it starts as computer use");
    if (!usesIt && c.kind !== "coding") throw fail("unsupported", "A GitHub link for this kind of task isn't supported");
    if (usesIt && link.type === "find" && !["project", "since_last_run"].includes(link.scope.scope)) throw fail("invalid_request", "A reviewer that uses the app tries all of it, or what changed since its last run");
    const settings = this.#settings(c.project_id);
    if (settings.mode === "off") throw fail("invalid_request", "Turn on the GitHub workflow for this project in Tasks › GitHub first");
    if (!this.#onGithub(c.project_id)) throw fail("not_found", "This project's `origin` isn't on github.com.");
    const github: TaskGithub = { link, rounds: 0 };
    const prefix = settings.branch_prefix;
    let title: string | null = null;
    let branch: TaskBranch | null = null;
    const read = (n: number, pr: boolean) => {
      const item = this.items.find((i) => i.number === n);
      if (!item) throw fail("not_found", `#${n} isn't open on GitHub`);
      if (item.pr !== pr) throw fail("invalid_request", pr ? `#${n} is an issue` : `#${n} is a pull request; link it as one`);
      if (!item.author_writes) github.from_stranger = true;
      return item;
    };
    switch (link.type) {
      case "issue": {
        const issue = read(link.number, false);
        github.issue = link.number;
        if (link.review_with) github.check_with = link.review_with;
        branch = { name: `${prefix}${link.number}-${slug(issue.title)}`, new: true };
        title = issue.title;
        break;
      }
      case "pr":
      case "check": {
        const item = read(link.number, true);
        const view = this.prs.get(link.number);
        github.pr = view ? structuredClone(view.summary) : {
          number: link.number, url: item.url, state: "open", draft: item.draft, head: "0".repeat(40), passed: 0, failed: 0, pending: 0, unresolved: 0, conflict: false,
        };
        if (link.type === "pr") {
          branch = { name: view?.head_branch ?? `${prefix}pr-${link.number}-${slug(item.title)}`, new: !view };
          github.stage = "babysitting";
        }
        title = link.type === "check" ? `Check #${link.number}: ${item.title}` : item.title;
        break;
      }
      case "stack": {
        const view = this.prs.get(link.number);
        if (!view || view.summary.state !== "open") throw fail("conflict", `Pull request #${link.number} isn't open`);
        github.base = view.head_branch;
        break;
      }
      case "find":
        if (link.max_issues != null && !(link.max_issues >= 1 && link.max_issues <= 20)) throw fail("invalid_request", "A finder run files 1 to 20 issues");
        break;
      case "new_pr":
      case "triage":
        break;
    }
    const plan = (["find", "triage", "check"].includes(link.type) && !usesIt) || !!github.from_stranger;
    return { github, title, branch, plan };
  }
}

/** Where a linked task's work goes: a taken-over pull request's own branch, else the one chosen, else the link's. */
export const githubTaskBranch = (prepared: PreparedLink | null, given: TaskBranch | null | undefined): TaskBranch | null => {
  const prBranch = prepared?.github.link.type === "pr" ? prepared.branch : null;
  if (prBranch && given && prBranch.name !== given.name) throw fail("invalid_request", `This pull request's work goes on \`${prBranch.name}\``);
  return prBranch ?? given ?? prepared?.branch ?? null;
};
