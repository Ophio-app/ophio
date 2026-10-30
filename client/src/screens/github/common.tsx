// SPDX-License-Identifier: Apache-2.0
//
// What a project's GitHub screens, Review and the PR sheet share: what
// stands in the way, sending an action or a new task with its outcome told,
// who works in each role, a list's rows, and the warning before the agent
// merges by itself.

import { useCallback, useState } from "react";
import { useCommandControl, type CommandControl } from "../../app/commandControl";
import { CircleDot, GitPullRequest, GitPullRequestDraft, ShieldAlert } from "lucide-react";
import { describeError, useApp, useHost } from "../../app/context";
import { shownLabels, useNeed } from "../../app/githubNeed";
import { STATUS_WORDS, choiceOf, draftGithubTask, lastRole, useGithubAct, useGithubSupported, warningText } from "../../app/github";
import type { AgentChoice } from "../../app/ui";
import { PRODUCT_NAME } from "../../brand";
import { announce } from "../../design/Announcer";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { InlineCode } from "../../design/InlineCode";
import { Pill, type Tone } from "../../design/Chip";
import { ListRow } from "../../design/ListRow";
import { Notice, availabilityText } from "../../design/Notice";
import { toast } from "../../design/Toast";
import { commandResult } from "../../host/commandResult";
import type {
  CheckState, Command, GithubAction, GithubItem, GithubLink, GithubMode, GithubProjectView, GithubSettings, HarnessInfo, ItemStatus, LaunchChoice, MergeState, Project, TaskMode, TaskPlacement,
} from "../../protocol/types";
import type { Platform } from "../../host/link";
import { NotAllowed, useMay } from "../../shell/NotAllowed";
import { agentChoiceBlocker, modeSupport, resolveAgent } from "../talk/intent";
import { agentUnavailable, preferredComputerAgent } from "../control/computerReview";
import "./github.css";

/** What stands between the person and GitHub here, else what's asked. */
export function Gate({ what, children }: { what: string; children: React.ReactNode }) {
  const { profile } = useApp();
  const supported = useGithubSupported();
  const allowed = useMay("tasks");
  if (!supported) {
    return (
      <Notice tone="warn" title="Update the computer">
        {profile.name} can’t link tasks to GitHub yet. Update {PRODUCT_NAME} on it to turn this on.
      </Notice>
    );
  }
  if (!allowed) return <NotAllowed what={what} permission="tasks" />;
  return <>{children}</>;
}

/** Sends one GitHub action; reports whether it went through. */
export type Run = (action: GithubAction, label: string, done?: string) => Promise<boolean>;

export const TURNING_ON = "Turning on GitHub";

/** While GitHub is off, the one thing to do here is turn it on. */
export function TurnOn({ settings, disabled }: { settings: GithubSettingsControl; disabled: boolean }) {
  return (
    <Notice tone="info" icon={GitPullRequest} title="GitHub is off for this project"
      action={<Button size="sm" variant="primary" disabled={disabled} pending={settings.turning === "drafts"}
        onClick={() => void settings.set({ ...settings.value, mode: "drafts" }, { label: TURNING_ON, what: "whether GitHub is on", done: "GitHub is on" })}>
        Turn on
      </Button>}>
      Turn it on to start issues and reviews from here. Agents open draft pull requests, keep their checks passing and say on GitHub that they did the work; you merge them.
    </Notice>
  );
}

export const asAgent = (c: LaunchChoice): AgentChoice => ({ harness: c.harness, model: c.model, effort: c.effort, ...(c.launch_policy ? { launchPolicy: c.launch_policy } : {}) });
export const asLaunch = (a: AgentChoice): LaunchChoice => ({ ...choiceOf(a), ...(a.launchPolicy ? { launch_policy: a.launchPolicy } : {}) });

/** The agent last chosen for `role` in this project on this phone, else what a new task would get. */
export function useRole(project: Project, role: "find" | "fix" | "review"): AgentChoice | null {
  const { profile } = useApp();
  const harnesses = useHost((s) => s.harnesses);
  const last = lastRole(profile.profileId, project.project_id, role);
  return resolveAgent(project, harnesses, last ? asAgent(last) : null);
}

/**
 * Who uses the app for a review: the agent last chosen for it here while it
 * still can use a screen, else the one computer use would choose, at its
 * default model. Null while none can.
 */
export function useUser(project: Project): AgentChoice | null {
  const { profile } = useApp();
  const harnesses = useHost((s) => s.harnesses);
  const last = lastRole(profile.profileId, project.project_id, "use");
  const managed = harnesses.filter((h) => h.integration === "managed");
  const h = managed.find((x) => x.kind === last?.harness && !agentUnavailable(x)) ?? preferredComputerAgent(managed, project);
  if (!h) return null;
  const model = h.kind === last?.harness && last.model && h.models.some((m) => m.id === last.model) ? last.model : null;
  return { harness: h.kind, model, effort: null, launchPolicy: null };
}

/** Why `agent` can't start a run in `mode`, or null. */
export function agentBlocker(harnesses: HarnessInfo[], agent: AgentChoice | null, mode: TaskMode): string | null {
  if (!agent) return "No agent on this computer can start work from the device yet. Check which agents are installed and signed in.";
  const h = harnesses.find((x) => x.kind === agent.harness);
  const why = agentChoiceBlocker(h, agent);
  if (why || mode === "work") return why;
  const m = modeSupport(h, mode).availability;
  return m.state === "available" ? null : availabilityText(m).detail ?? `${h?.display_name ?? "This agent"} can’t run read-only.`;
}

/** An agent as sent: its launch flags only to a computer that applies them. */
export const agentSent = (agent: AgentChoice, launchSupported: boolean): AgentChoice =>
  launchSupported ? agent : { ...agent, launchPolicy: null };

/** Next up, starting by itself once the project is free. */
export const nextUp = (): TaskPlacement => ({ mode: "queue", position: null, authorization: "auto_start" });

/** Now, or into Next up while another agent works in the project. */
export const startOrQueue = (waits: boolean): TaskPlacement => (waits ? nextUp() : { mode: "start_now" });

/** What a linked task is asked, with the person's own direction after it. */
export const withDirection = (text: string, direction: string): string =>
  direction.trim() ? `${text}\n\n${direction.trim()}` : text;

/** Sends one action, says how it went, and reports whether it went through. */
export function useGithubRun() {
  const act = useGithubAct();
  const [busy, setBusy] = useState<string | null>(null);
  const run = useCallback(async (action: GithubAction, label: string, done?: string, before?: () => Promise<void>): Promise<boolean> => {
    setBusy(label);
    try {
      if (before) await before();
      await act(action, label);
      if (done) toast(done);
      return true;
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), { tone: "stop" });
      return false;
    } finally { setBusy(null); }
  }, [act]);
  return { busy, run };
}

/** Settings compared as the computer keeps them; the warning's answer is its own. */
const settingsKey = ({ warning_answered: _a, warned_repo: _r, ...rest }: GithubSettings) => stable(rest);
const stable = (v: unknown): string => Array.isArray(v) ? `[${v.map(stable).join(",")}]`
  : v && typeof v === "object" ? `{${Object.keys(v).sort().map((k) => `${k}:${stable((v as Record<string, unknown>)[k])}`).join(",")}}`
  : JSON.stringify(v) ?? "";

export interface GithubSettingsControl {
  /** The settings with the person's latest choices in them. */
  value: GithubSettings;
  /** The settings in force: what the on and merge switches show. */
  inForce: GithubSettings;
  /** The mode on its way, once it takes a moment; null otherwise. */
  turning: GithubMode | null;
  set: CommandControl<GithubSettings>["set"];
}

/**
 * A project's GitHub settings, which the computer keeps as one record and
 * replaces whole. Changes go one at a time, each with every change made
 * before it, so a quick second tap never undoes the first. The rest show the
 * person's choice at once; whether GitHub is on, and whether the agent
 * merges, show what is in force until the computer has the change.
 */
export function useGithubSettings(view: GithubProjectView | null, reload: () => void): GithubSettingsControl {
  const act = useGithubAct();
  const inForce = view?.settings ?? DEFAULT_SETTINGS;
  const control = useCommandControl<GithubSettings>({
    kind: "instant",
    value: inForce,
    equal: (a, b) => settingsKey(a) === settingsKey(b),
    describe: (e) => (e instanceof Error ? e.message : String(e)),
    send: async (settings, note) => {
      try { await act({ action: "set_settings", project_id: view!.project_id, settings }, note.label); } finally { reload(); }
    },
  });
  const turning = control.waiting && control.waiting.mode !== inForce.mode ? control.waiting.mode : null;
  return { value: control.value, inForce, turning, set: control.set };
}

// Stands in until the view is read; nothing shows a setting before then.
const DEFAULT_SETTINGS: GithubSettings = {
  mode: "off", branch_prefix: "", label: false, warning_answered: false,
  workflow: { fix_findings: "ask_first", findings_per_run: 5, check_before_merge: false },
};

/**
 * Starts a task from GitHub without opening Review: sends it, waits for the
 * computer to make it, says how it went, with a way to open it, and reports
 * whether it did.
 */
export function useStartTask() {
  const { session, profile, platform, ui } = useApp();
  const [sending, setSending] = useState<string | null>(null);
  const start = useCallback(async (body: Command, label: string, done: string): Promise<boolean> => {
    platform.haptic?.("tap");
    setSending(label);
    try {
      const made = await commandResult(session, await session.command(body, { label }));
      const projectId = "project_id" in body ? body.project_id : null;
      toast(done, made?.kind === "task_created" && projectId
        ? { action: { label: "Open task", run: () => ui.openTask(projectId, made.task_id) } }
        : {});
      return true;
    } catch (e) {
      toast(describeError(e, profile.name), { tone: "stop" });
      return false;
    } finally { setSending(null); }
  }, [session, profile.name, platform, ui]);
  return { sending, start };
}

/** What a person hears once GitHub work is drafted. */
export const draftedWords = ({ setAside }: { setAside: boolean }) =>
  setAside ? "Your earlier words are in Saved drafts. Review this before starting." : "Review it before starting.";

/**
 * Puts GitHub work in the project's new-task draft and opens Review, saying
 * so; reports whether it did.
 */
export function useDraftGithub(projectId: string) {
  const { ui } = useApp();
  return useCallback((d: { link: GithubLink; text?: string; agent?: AgentChoice | null; plan?: boolean }): boolean => {
    try {
      announce(draftedWords(draftGithubTask(ui, projectId, d)));
      return true;
    } catch (e) {
      toast(e instanceof Error ? e.message : String(e), { tone: "stop" });
      return false;
    }
  }, [ui, projectId]);
}

const STATUS_TONE: Record<ItemStatus, Tone> = { agent_working: "accent", queued: "neutral", stopped: "warn", waiting_for_you: "warn", green: "ok" };

const itemIcon = (item: GithubItem) => (!item.pr ? CircleDot : item.draft ? GitPullRequestDraft : GitPullRequest);

const CHECK_PILL: Record<CheckState, string> = { pending: "Checks running", passed: "Checks pass", failed: "Checks fail", other: "Checks didn’t run" };
const CHECK_TONE: Record<CheckState, Tone> = { pending: "neutral", passed: "ok", failed: "stop", other: "neutral" };
export const MERGE_WORDS: Record<MergeState, string> = { ready: "Can merge", blocked: "Can’t merge yet", behind: "Out of date", conflict: "Conflicts" };
const MERGE_TONE: Record<MergeState, Tone> = { ready: "ok", blocked: "warn", behind: "neutral", conflict: "stop" };

/**
 * An issue or pull request in a list: number, title, who, when and how it
 * stands. A pull request also says how its checks went, unless the task on it
 * already does, and whether GitHub would merge it.
 */
/**
 * An issue or pull request in a list. One that waits for the person says why
 * in a sentence, with its next step as the pill, in place of the Sort's note.
 * While pull requests are being picked, it's ticked or not, and a tap picks it.
 */
export function ItemRow({ item, onOpen, picked }: { item: GithubItem; onOpen: () => void; picked?: boolean }) {
  const need = useNeed(item);
  const checks = item.pr && item.checks && item.status !== "green" ? item.checks : null;
  const merge = item.pr && !item.draft ? item.merge ?? null : null;
  const labels = shownLabels(item.labels);
  return (
    <ListRow icon={itemIcon(item)} onClick={onOpen} chevron={picked === undefined} ticked={picked}
      title={`#${item.number} ${item.title}`}
      subtitle={<>
        @{item.author}{labels.length ? ` · ${labels.join(", ")}` : ""} · <Age at={item.updated_at} suffix=" ago" />
        {need ? <span className="github-item__note github-item__need">{need.row || need.headline}</span>
          : item.triage_note && <span className="github-item__note">{item.triage_note}</span>}
      </>}
      meta={(need || item.status || checks || merge) && (
        <span className="github-item__pills">
          {need ? <Pill tone="warn">{need.pill}</Pill>
            : item.status && <Pill tone={STATUS_TONE[item.status]}>{STATUS_WORDS[item.status]}</Pill>}
          {checks && <Pill tone={CHECK_TONE[checks]}>{CHECK_PILL[checks]}</Pill>}
          {merge && <Pill tone={MERGE_TONE[merge]}>{MERGE_WORDS[merge]}</Pill>}
        </span>
      )} />
  );
}

/**
 * Asks for the phone's own unlock before a merge or Mark ready. Without a way
 * to ask, the action doesn't go ahead.
 */
export async function unlockFor(platform: Platform, title: string): Promise<void> {
  const lock = platform.appLock;
  if (!lock?.confirm) throw new Error("This phone can’t confirm with its screen lock, so nothing was sent. Do it on GitHub instead.");
  await lock.confirm(title);
}

/** The computer's words, `code` in them shown as code. */
export function WithCode({ text }: { text: string }) {
  return <>{text.split("`").map((part, i) => (i % 2 ? <InlineCode key={i}>{part}</InlineCode> : part))}</>;
}

/** What the computer found risky about merging. */
export function MergeWarnings({ warnings }: { warnings: string[] }) {
  if (warnings.length === 0) return null;
  return <ul>{warnings.map((w) => <li key={w}><WithCode text={w} /></li>)}</ul>;
}

/**
 * Shown while merging once checks pass is on but its warning isn't answered, as
 * when it was turned on at the computer; nothing merges until it is.
 */
export function ThroughMergeWarning({ view, busy, online, onAnswer }: {
  view: GithubProjectView; busy: string | null; online: boolean; onAnswer: (keep: boolean) => void;
}) {
  const branch = view.repo?.default_branch ?? "main";
  const repo = repoName(view);
  return (
    <Notice tone="warn" icon={ShieldAlert} title="Before it merges by itself" className="github-warning">
      <p>{warningText(branch, repo || "this repository")}</p>
      <MergeWarnings warnings={view.warnings} />
      <p>Nothing merges until you choose.</p>
      <div className="github-warning__actions">
        <Button variant="primary" size="sm" disabled={!online || !!busy || !repo} pending={busy === KEEP} onClick={() => onAnswer(true)}>Merge once checks pass</Button>
        <Button variant="secondary" size="sm" disabled={!online || !!busy} pending={busy === DRAFTS} onClick={() => onAnswer(false)}>I’ll merge them</Button>
      </div>
    </Notice>
  );
}

const KEEP = "Letting the agent merge once checks pass";
const DRAFTS = "Leaving merging to you";

/** The repository a view was read for, as `owner/name`; empty when it isn't on GitHub. */
const repoName = (view: GithubProjectView) => view.repo ? `${view.repo.owner}/${view.repo.name}` : "";

/** The warning's answer for the repository it was shown for, as an action and its label. */
export const answerWarning = (view: GithubProjectView, keep: boolean) => ({
  action: { action: "answer_warning", project_id: view.project_id, repo: repoName(view), keep_through_merge: keep } as const,
  label: keep ? KEEP : DRAFTS,
  done: keep ? "The agent merges its pull requests once their checks pass" : "You merge the agent’s pull requests",
});
