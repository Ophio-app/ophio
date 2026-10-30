// SPDX-License-Identifier: Apache-2.0
//
// Run all, at the top of the Issues tab: one tap fixes every issue in Agent
// can take it, one after another, with the coder and reviewer chosen once
// for the project behind the cog. Issues nobody has sorted are sorted first,
// and what the Sort can give the agent joins the run. It asks nothing more
// and keeps going until none is left. Pause stops the fix working now and
// holds the rest until Resume carries them on; Stop ends the run and takes
// the fixes still waiting out of Next up. The run is kept on the computer,
// so every device shows the same.

import { useState } from "react";
import { Bot, ChevronRight, Eye, ListChecks, Pause, Play, SlidersHorizontal, Square } from "lucide-react";
import { shallowEqual, useApp, useHost, useLink } from "../../app/context";
import { choiceOf, linkText } from "../../app/github";
import { GITHUB_RUN_FEATURE, GITHUB_RUN_UPDATE } from "../../app/githubFeature";
import { LAUNCH_POLICY_FEATURE } from "../../app/launchPolicy";
import { LaunchLine } from "../talk/LaunchLine";
import type { AgentChoice } from "../../app/ui";
import { useWarning } from "../../app/warnings";
import { Button, IconButton } from "../../design/Button";
import { ListRow } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import type { GithubItem, GithubLink, GithubProjectView, LaunchChoice, Project, Task } from "../../protocol/types";
import { going, itemStatus } from "../../state/githubStatus";
import { projectTasks } from "../../state/projection";
import { NotAllowed, useMay } from "../../shell/NotAllowed";
import { AgentSheet } from "../talk/AgentSheet";
import { agentLabel, createTaskCommand, writerHold, writerHoldText } from "../talk/intent";
import { pendingQueueItems, useQueue } from "../work/useQueue";
import { agentBlocker, asAgent, asLaunch, useGithubRun, useRole, useStartTask } from "./common";
import { useReviewer } from "./Reviewer";

const RUNNING = "Running all the issues";
const PAUSING = "Pausing Run all";
const RESUMING = "Resuming Run all";
const STOPPING = "Stopping Run all";
const SORTING = "Sorting open issues";
const SAVING = "Saving Run all’s defaults";

/** Next up's reasons for a task behind one still running: the computer's, and the simulator's. */
const WAITING_TURN = ["Waiting for a prerequisite", "Waiting for an earlier task"];

/** Where one of the run's fixes is. */
type Stage = "working" | "waiting" | "stopped" | "needs" | "done" | "out";

function stageOf(task: Task | undefined): Stage {
  if (!task || task.state === "archived") return "out";
  if (task.state === "dispatching" || task.state === "working") return "working";
  if (task.state === "accepted") return "waiting";
  if (task.state === "interrupted") return "stopped";
  return itemStatus(task) === "waiting_for_you" ? "needs" : "done";
}

const STAGE_WORDS: Record<Stage, string> = {
  working: "working", waiting: "waiting", stopped: "stopped", needs: "needs you", done: "done", out: "taken out",
};

const issues = (n: number) => (n === 1 ? "1 issue" : `${n} issues`);
/** `#1`, `#1 and #2`, `#1, #2 and 3 more`. */
function numbers(ns: number[]): string {
  const shown = [...ns.slice(0, 3).map((n) => `#${n}`), ...(ns.length > 3 ? [`${ns.length - 3} more`] : [])];
  return shown.length > 1 ? `${shown.slice(0, -1).join(", ")} and ${shown.at(-1)}` : shown[0] ?? "";
}

/**
 * The Run all card. `take` and `unsorted` are the list as shown: what the
 * agent can take, and what nobody has sorted and no task works yet.
 */
export function RunAll({ project, view, take, unsorted, off }: {
  project: Project; view: GithubProjectView; take: GithubItem[]; unsorted: GithubItem[]; off: boolean;
}) {
  const { ui, platform } = useApp();
  const online = useLink().state === "online";
  const supported = useHost((s) => !!s.capabilities?.features.includes(GITHUB_RUN_FEATURE));
  const allowed = useMay("tasks");
  const harnesses = useHost((s) => s.harnesses);
  const { queue } = useQueue(project.project_id);
  const { busy, run: act } = useGithubRun();
  const { sending, start } = useStartTask();
  const [defaults, setDefaults] = useState<"edit" | "then_run" | null>(null);
  const [stopping, setStopping] = useState(false);
  const stopWarning = useWarning("issues.stop");
  const fallbackSorter = useRole(project, "find");
  const headId = pendingQueueItems(queue)[0]?.task_id;
  const headTask = useHost((s) => (headId ? s.tasks[headId] : undefined));

  const run = view.run;
  const active = !!run && run.ended_at == null;
  const fixes = run?.fixes ?? [];
  const tasks = useHost((s) => fixes.map((f) => s.tasks[f.task_id]), shallowEqual);
  const sortTask = useHost((s) => projectTasks(s, project.project_id).find((t) => t.github?.link.type === "triage" && going(t)));
  const writer = useHost((s) => writerHold(s.projects[project.project_id]?.active_writer, s), shallowEqual);
  const flow = view.settings.workflow;
  const fixWith = flow.fix_with ?? null;

  if (!supported) return <Notice tone="info">{GITHUB_RUN_UPDATE}</Notice>;

  const stages = tasks.map(stageOf);
  const count = (stage: Stage) => stages.filter((s) => s === stage).length;
  const numbered = (stage: Stage) => fixes.filter((_, i) => stages[i] === stage).map((f) => f.number);
  const working = numbered("working");
  const waiting = count("waiting");
  const stopped = numbered("stopped");
  const counted = fixes.length - count("out");
  const finished = count("done") + count("needs");
  const paused = active && (run!.paused || !!queue?.paused);
  const sorting = !!sortTask;

  // What Run all would take on now: each issue the agent can take that no
  // task works yet or that waits in Next up, and the issues still to sort.
  const toFix = take.filter((i) => !i.task_id || i.status === "queued").length;
  const toSort = unsorted.length;
  const can = online && allowed && !off && !busy;
  const fixBlocker = fixWith ? agentBlocker(harnesses, asAgent(fixWith), "work") : null;

  const go = () => {
    platform.haptic?.("tap");
    if (!fixWith) { setDefaults("then_run"); return; }
    void act({ action: "run_issues", project_id: project.project_id, step: "start" }, RUNNING,
      toFix ? `Running all: ${issues(toFix)}${toSort ? `, after sorting ${toSort}` : ""}` : `Sorting ${issues(toSort)}, then fixing what the agent can take`);
  };
  const step = (s: "pause" | "resume" | "stop", label: string, done: string) => {
    platform.haptic?.("tap");
    void act({ action: "run_issues", project_id: project.project_id, step: s }, label, done);
  };
  const stop = () => step("stop", STOPPING, working.length ? `Stopped ${numbers(working)}; Run all is off` : "Run all is off");
  // Sort alone: the reviewer sorts if there is one, else the coder, else the
  // agent last used here. Nothing starts from it but what Run all takes on.
  const sorter = flow.review_with ?? fixWith;
  const sortAgent: AgentChoice | null = sorter ? { ...asAgent(sorter), launchPolicy: null } : fallbackSorter;
  const sortBlocker = agentBlocker(harnesses, sortAgent, "plan");
  const sort = () => {
    if (!sortAgent) return;
    const link: GithubLink = { type: "triage" };
    void start(createTaskCommand({
      projectId: project.project_id, prompt: linkText(link), title: null, agent: sortAgent, mode: "plan",
      placement: { mode: "start_now" }, mission: null, bundle: null, github: link,
    }), SORTING, `Sorting ${issues(toSort)}`);
  };

  // Next up starts one task at a time, so a task ahead of the fixes that
  // waits for a go-ahead, or behind one that failed, holds them all. Say so,
  // where the fixes are; one simply waiting its turn holds nothing.
  const head = pendingQueueItems(queue)[0];
  const asks = head?.state.state === "awaiting_start";
  const heldBy = active && !paused && head && !fixes.some((f) => f.task_id === head.task_id) && headTask
    && (asks || (head.state.state === "blocked" && !WAITING_TURN.includes(head.state.reason))) ? headTask : null;
  const heldByWriter = active && !paused && !working.length && waiting > 0 && writer?.task
    && !fixes.some((f) => f.task_id === writer.task!.task_id) ? writer : null;

  const title = !active ? "Run all"
    : paused ? "Paused"
    : working.length ? `Fixing ${numbers(working)}`
    : sorting && !waiting ? "Sorting new issues"
    : waiting ? "Waiting to start"
    : "Finishing";
  const line = !active
    ? [toFix && `${issues(toFix)} to fix`, toSort && (sorting ? `sorting ${toSort}` : `${toSort} to sort first`)].filter(Boolean).join(" · ")
      || (sorting ? "Sorting new issues" : "Nothing to fix right now")
    : paused
      ? run!.paused
        ? [stopped.length && `${numbers(stopped)} ${stopped.length === 1 ? "carries" : "carry"} on at Resume`, waiting && `${waiting} waiting`].filter(Boolean).join(" · ") || "Resume carries on"
        : queue?.pause_reason ?? "Next up is paused. Resume carries on."
      : [counted && `${finished} of ${counted} done`, waiting && `${waiting} waiting`, sorting && "sorting new issues"].filter(Boolean).join(" · ");
  const who = fixWith
    ? `Fixed by ${agentLabel(harnesses, asAgent(fixWith))}${flow.review_with ? ` · reviewed by ${agentLabel(harnesses, asAgent(flow.review_with))}` : ""}`
    : "Choose who fixes them once; then one tap runs them all.";
  const last = !active && run && run.fixes.length > 0
    ? `Last run${run.stopped ? ", stopped" : ""}: ${[count("done") && `${count("done")} done`, count("needs") && `${count("needs")} need you`,
      stopped.length && `${stopped.length} stopped`].filter(Boolean).join(", ") || `${issues(run.fixes.length)}`}`
    : null;

  return (
    <section className={`settings-card run-all${active ? " is-active" : ""}`} aria-labelledby="run-all-title">
      <div className="run-all__head">
        <div className="run-all__text">
          <h3 id="run-all-title" className="run-all__title">{title}</h3>
          <p className="settings-meta" role="status">{line}</p>
        </div>
        <IconButton icon={SlidersHorizontal} label="Run all defaults" onClick={() => setDefaults("edit")} />
      </div>
      {active && counted > 0 && (
        <div className="run-all__bar" role="progressbar" aria-label="Fixes done" aria-valuemin={0} aria-valuemax={counted} aria-valuenow={finished}>
          <span style={{ transform: `scaleX(${finished / counted})` }} />
        </div>
      )}
      {active && fixes.length > 0 && (
        <ul className="run-all__fixes" aria-label="Run all’s fixes">
          {fixes.map((f, i) => (
            <li key={f.task_id}>
              <button type="button" className={`run-all__fix run-all__fix--${stages[i]}`} disabled={!tasks[i]}
                aria-label={`#${f.number}, ${STAGE_WORDS[stages[i]!]}`} onClick={() => tasks[i] && ui.openTask(project.project_id, f.task_id)}>
                #{f.number}
              </button>
            </li>
          ))}
        </ul>
      )}
      {heldBy && head && (
        <Notice tone="warn" title="Held up in Next up"
          action={asks
            ? <Button size="sm" onClick={() => ui.push({ screen: "review", projectId: project.project_id, taskId: head.task_id, queueItemId: head.item_id })}>Review and start</Button>
            : <Button size="sm" onClick={() => ui.openTask(project.project_id, head.task_id)}>Open it</Button>}>
          “{heldBy.title}” is first in Next up and {head.state.state === "blocked" ? head.state.reason.toLowerCase() : "waits for your go-ahead"}. The fixes start after it.
        </Notice>
      )}
      {heldByWriter && (heldByWriter.waiting || heldByWriter.unresolved
        // It waits on the person: take them to it.
        ? <Notice tone="warn" title="Held up by another task"
            action={<Button size="sm" onClick={() => ui.openTask(project.project_id, heldByWriter.task!.task_id)}>Open it</Button>}>
            {writerHoldText(heldByWriter, project.name)}
          </Notice>
        : <p className="settings-meta" role="status">{writerHoldText(heldByWriter, project.name)}</p>)}
      {!active && <p className="settings-meta">{who}</p>}
      {last && <p className="settings-meta">{last}</p>}
      {!allowed ? <NotAllowed what="run the issues" permission="tasks" />
        : off ? <p className="settings-meta" role="status">Turn GitHub on for this project first.</p>
        : fixBlocker && !active ? <p className="settings-meta" role="status">{fixBlocker}</p>
        : null}
      <div className="run-all__actions">
        {!active && <>
          {/* A Sort under way is a task like any other: it opens, to watch.
              It is done when this turns back into Sort, or goes. */}
          {sortTask ? (
            <Button variant="secondary" icon={ListChecks} trailingIcon={ChevronRight} aria-label={`Sorting${toSort ? ` ${issues(toSort)}` : ""}: open it`}
              onClick={() => ui.openTask(project.project_id, sortTask.task_id)}>
              Sorting
            </Button>
          ) : toSort > 0 && (
            <Button variant="secondary" icon={ListChecks} disabled={!can || !!sortBlocker || !!sending} pending={sending === SORTING}
              title={sortBlocker ?? undefined} onClick={sort}>
              Sort
            </Button>
          )}
          <Button variant="primary" icon={Play} disabled={!can || (toFix === 0 && toSort === 0) || !!fixBlocker} pending={busy === RUNNING} onClick={go}>
            Run all
          </Button>
        </>}
        {active && !paused && <>
          <Button variant="secondary" icon={Square} disabled={!can} pending={busy === STOPPING}
            onClick={() => stopWarning.ask(() => setStopping(true), stop)}>Stop{stopWarning.more}</Button>
          <Button variant="primary" icon={Pause} disabled={!can} pending={busy === PAUSING}
            onClick={() => step("pause", PAUSING, working.length ? `Paused; ${numbers(working)} ${working.length === 1 ? "carries" : "carry"} on at Resume` : "Run all is paused")}>Pause</Button>
        </>}
        {paused && <>
          <Button variant="secondary" icon={Square} disabled={!can} pending={busy === STOPPING}
            onClick={() => stopWarning.ask(() => setStopping(true), stop)}>Stop{stopWarning.more}</Button>
          <Button variant="primary" icon={Play} disabled={!can} pending={busy === RESUMING}
            onClick={() => step("resume", RESUMING, "Run all carries on")}>Resume</Button>
        </>}
      </div>
      <Sheet open={stopping} onClose={() => setStopping(false)} title="Stop Run all?"
        footer={<Button variant="warn" disabled={!can} onClick={() => { stopWarning.agreed(); setStopping(false); stop(); }}>Stop</Button>}>
        <div className="settings-stack">
          <p>{[
            working.length ? `${numbers(working)} ${working.length === 1 ? "stops now, where it is" : "stop now, where they are"}.` : "",
            waiting ? `The ${issues(waiting)} waiting ${waiting === 1 ? "is" : "are"} taken out of Next up.` : "",
            "Run all takes them up again.",
          ].filter(Boolean).join(" ")}</p>
          {stopWarning.check}
        </div>
      </Sheet>
      {defaults && (
        <RunDefaults project={project} view={view} then={defaults === "then_run"} onClose={() => setDefaults(null)}
          onSaved={(ran) => { setDefaults(null); if (ran) toast(`Running all: ${issues(toFix)}${toSort ? `, after sorting ${toSort}` : ""}`); }} />
      )}
    </section>
  );
}

/**
 * Run all's defaults for the project, kept on the computer: who fixes each
 * issue, and who reviews each fix and sorts new issues, if anyone.
 */
function RunDefaults({ project, view, then, onClose, onSaved }: {
  project: Project; view: GithubProjectView; then: boolean; onClose: () => void; onSaved: (ran: boolean) => void;
}) {
  const online = useLink().state === "online";
  const admin = useMay("admin");
  const harnesses = useHost((s) => s.harnesses);
  const launchSupported = useHost((s) => !!s.capabilities?.features.includes(LAUNCH_POLICY_FEATURE));
  const { busy, run: act } = useGithubRun();
  const flow = view.settings.workflow;
  const lastFixer = useRole(project, "fix");
  const [fixChosen, setFix] = useState<AgentChoice | null>(null);
  const fixer = fixChosen ?? (flow.fix_with ? asAgent(flow.fix_with) : lastFixer);
  const fallback = useReviewer(project, fixer).agent;
  const [reviewChosen, setReview] = useState<AgentChoice | null>(null);
  const reviewer = reviewChosen ?? (flow.review_with ? asAgent(flow.review_with) : fallback);
  const [reviewOn, setReviewOn] = useState(flow.fix_with ? !!flow.review_with : true);
  const [choosing, setChoosing] = useState<"fix" | "review" | null>(null);
  const blocker = agentBlocker(harnesses, fixer, "work") ?? (reviewOn ? agentBlocker(harnesses, reviewer, "plan") : null);

  const save = async () => {
    if (!fixer) return;
    const fix_with: LaunchChoice = launchSupported ? asLaunch(fixer) : choiceOf(fixer);
    const { review_with: _, ...rest } = flow;
    const workflow = { ...rest, fix_with, ...(reviewOn && reviewer ? { review_with: choiceOf(reviewer) } : {}) };
    const saved = await act({ action: "set_settings", project_id: project.project_id, settings: { ...view.settings, workflow } }, SAVING,
      then ? undefined : "Run all’s defaults are saved");
    if (!saved) return;
    const ran = then && await act({ action: "run_issues", project_id: project.project_id, step: "start" }, RUNNING);
    onSaved(ran);
  };

  return (
    <Sheet open onClose={onClose} title="Run all defaults" subtitle={project.name}
      footer={<Button variant="primary" disabled={!online || !admin || !fixer || !!blocker || !!busy} pending={!!busy} onClick={() => void save()}>
        {then ? "Save and run all" : "Save"}
      </Button>}>
      <div className="settings-stack">
        <p className="settings-meta">Run all fixes each issue in Agent can take it with these, one after another, and asks nothing more.</p>
        <div className="group">
          <ListRow icon={Bot} title="Fixed by" subtitle={fixer ? agentLabel(harnesses, fixer) : "No agent can start work yet"} chevron
            disabled={!fixer} onClick={() => setChoosing("fix")} />
        </div>
        {fixer && <LaunchLine className="settings-meta" harness={fixer.harness} policy={launchSupported ? fixer.launchPolicy : null} />}
        <label className="settings-check">
          <input type="checkbox" checked={reviewOn} disabled={!reviewer} onChange={(e) => setReviewOn(e.target.checked)} />
          Have another agent review each fix
        </label>
        <p className="settings-meta">
          {reviewOn
            ? "Once a fix’s pull request passes its checks, this agent reviews the change without editing files. It also sorts new issues."
            : "Off: uses this project’s setting, and the agent fixing them sorts new issues."}
        </p>
        {reviewOn && reviewer && (
          <div className="group">
            <ListRow icon={Eye} title="Reviewed by" subtitle={agentLabel(harnesses, reviewer)} chevron onClick={() => setChoosing("review")} />
          </div>
        )}
        {blocker ? <p className="settings-meta" role="status">{blocker}</p>
          : !admin && <p className="settings-meta" role="status">This device may not change the project’s GitHub settings.</p>}
      </div>
      {fixer && (
        <AgentSheet open={choosing === "fix"} onClose={() => setChoosing(null)} project={project} value={fixer} mode="work"
          launchFlags={launchSupported} onChange={setFix} />
      )}
      {reviewer && (
        <AgentSheet open={choosing === "review"} onClose={() => setChoosing(null)} project={project} value={reviewer} mode="plan"
          launchFlags={false} onChange={setReview} />
      )}
    </Sheet>
  );
}
