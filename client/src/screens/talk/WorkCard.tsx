// SPDX-License-Identifier: Apache-2.0
//
// The work a chat's turn suggested, as one card under that turn (Design A).
// One suggestion is a title, one plain line, a chip naming the agent, model
// and effort it starts with, and Start; several are one card that opens to a
// short list. The line says what Start makes: the kind of task, its branch
// and pull request, what it may reach and when it starts, and anything past
// the project is written out beneath it, never behind the opener.
//
// The chip is the person's usual choice for that kind of task, and tapping it
// opens the same agent picker the message box uses, small, over the chat;
// a pick shows on the card at once and is remembered for that kind. Beneath
// it, More options opens Review with everything filled in, and Do it here has
// the chat's own agent do the work in the chat, after the computer confirms
// it may change the project. Start flips the card at once and goes back,
// saying why, if the computer refuses.
//
// An ordered set is a card of its own ("4 tasks, in order") that opens to the
// numbered list; Start all puts them in Next up so each starts when the one
// before has finished. What its tasks share (the agent, and what kind of task
// each is) is said once for the set; each step keeps its title and what
// differs. When a step doesn't succeed, the computer puts the ones after it
// away unstarted: the card shows that step as it stands, offers no Start all
// past it, and lets the person start any later step on its own.
// Fixing every open issue is a card for the project's
// Run all, with its own fixer and reviewer. A task Ophio started by itself,
// on the project's go-ahead, says so on its row; tapping it opens the task,
// where Stop is.

import { ChevronDown, ChevronRight, ListOrdered, MessageSquare, Play, SlidersHorizontal } from "lucide-react";
import { useCallback, useId, useMemo, useRef, useState } from "react";
import { describeError, shallowEqual, useApp, useHost, useLink, useUi } from "../../app/context";
import { draftNewTask, useGithubProject } from "../../app/github";
import { rememberedPr, rememberUsual, usualAgent } from "../../app/launchMemory";
import { draftKey, type AgentChoice, type Draft } from "../../app/ui";
import { Button } from "../../design/Button";
import { Pill } from "../../design/Chip";
import { ListRow } from "../../design/ListRow";
import { toast } from "../../design/Toast";
import { commandResult } from "../../host/commandResult";
import type { Project, Run, Task, TaskOrigin } from "../../protocol/types";
import { goalKey } from "../control/computerReview";
import { useSignInOn } from "../settings/signIn";
import { accessHeld, covers, heldAccess, normalizedAccess } from "./access";
import { isTaskRunning } from "../../state/projection";
import { useOpenTask } from "../work/shared";
import { cardLive } from "./cardLive";
import { AgentChip } from "./AgentChip";
import { AgentSheet } from "./AgentSheet";
import { agentLabel, writerHold } from "./intent";
import { REVIEWED_FEATURE, SETS_FEATURE, asksMore, cardKind, groups, kindMode, leastAccess, planCard, places, type CardKind, type CardPlan, type Suggestion } from "./workCards";
import { RunAllCard } from "./RunAllCard";
import "./workCards.css";

type Local = { phase: "starting" } | { phase: "started"; taskId: string | null } | { phase: "here" };
/** Which chip's picker is open: the whole set's, or one task's. */
type Picking = { scope: "set" } | { scope: "row"; i: number } | null;

const sameChoice = (a: AgentChoice, b: AgentChoice) => a.harness === b.harness && a.model === b.model && a.effort === b.effort && (a.launchPolicy ?? null) === (b.launchPolicy ?? null);

/** The cards for one turn's suggestions, or nothing when it suggested none:
 * one for those that stand alone, one for each ordered set, and one for
 * each proposal to fix every issue. */
export function WorkCard({ chat, run, project }: { chat: Task; run: Run; project: Project }) {
  const all = (run.outcome?.report?.new_work ?? NO_WORK) as Suggestion[];
  const g = useMemo(() => groups(all), [all]);
  if (!all.length || chat.state === "archived") return null;
  return <>
    {g.sets.map((set) => <Card key={`set-${set[0]}`} chat={chat} run={run} project={project} all={all} indices={set} ordered />)}
    {g.loose.length > 0 && <Card chat={chat} run={run} project={project} all={all} indices={g.loose} ordered={false} />}
    {g.allIssues.map((i) => <RunAllCard key={`all-${i}`} chat={chat} run={run} project={project} index={i} />)}
  </>;
}
const NO_WORK: Suggestion[] = [];

function Card({ chat, run, project, all, indices, ordered }: {
  chat: Task; run: Run; project: Project; all: Suggestion[]; indices: number[]; ordered: boolean;
}) {
  const suggestions = useMemo(() => indices.map((i) => all[i]!), [all, indices]);
  const app = useApp();
  const { session, ui, profile } = app;
  const heading = useId();
  const pid = project.project_id;
  const online = useLink().state === "online";
  const projects = useHost((s) => s.projects);
  const harnesses = useHost((s) => s.harnesses);
  const features = useHost((s) => s.capabilities?.features ?? NONE);
  const held = useHost((s) => accessHeld(s.capabilities));
  const mayStart = useHost((s) => !!s.grants?.tasks);
  const writer = useHost((s) => {
    const w = writerHold(s.projects[pid]?.active_writer, s);
    return w ? { title: w.task?.title ?? null } : null;
  }, shallowEqual);
  // A task already started from one of these, on this phone or another; one
  // the computer put away before it started doesn't count.
  const started = useHost((s) => {
    const out: Record<number, string> = {};
    for (const t of Object.values(s.tasks)) {
      if (t.origin?.chat !== chat.task_id || t.origin.run !== run.run_id || putAway(t)) continue;
      const k = indices.indexOf(t.origin.index);
      if (k >= 0) out[k] = t.task_id;
    }
    return out;
  }, shallowEqual);
  // Those the computer put away before they started: their ids, and places.
  const away = useHost((s) => {
    const out: Record<string, number> = {};
    for (const t of Object.values(s.tasks)) {
      if (t.origin?.chat !== chat.task_id || t.origin.run !== run.run_id || !putAway(t)) continue;
      const k = indices.indexOf(t.origin.index);
      if (k >= 0) out[t.task_id] = k;
    }
    return out;
  }, shallowEqual);
  // The first step of a set that didn't succeed: nothing after it starts in order.
  const stopped = useHost((s) => {
    if (!ordered) return null;
    for (let k = 0; k < indices.length; k++) {
      const t = Object.values(s.tasks).find((t) => t.origin?.chat === chat.task_id && t.origin.run === run.run_id && t.origin.index === indices[k] && !putAway(t));
      if (t && !succeeded(t, s.runs)) return k;
      if (!t && Object.values(s.tasks).some((t) => t.origin?.chat === chat.task_id && t.origin.run === run.run_id && t.origin.index === indices[k])) return k;
    }
    return null;
  });
  const paused = useHost((s) => !!s.queues[pid]?.paused);
  const signInOn = useSignInOn();
  const wantsGithub = suggestions.some((w) => w.issue != null || w.mode === "work");
  const github = useGithubProject(wantsGithub && project.vcs?.kind === "git" && online ? pid : null);
  const githubView = project.vcs?.kind !== "git" ? null : github.view ?? (github.error || !online ? null : undefined);
  const reviewer = githubView?.settings.workflow.review_with ?? null;
  const pr = rememberedPr(profile.profileId, pid);

  const kinds = useMemo(() => suggestions.map(cardKind), [suggestions]);
  const targets = useMemo(() => suggestions.map((w) => places(w, project, projects).target ?? project), [suggestions, project, projects]);
  const drafts = useUi((s) => s.drafts);
  const savedDrafts = useUi((s) => s.savedDrafts);
  const draftFor = (i: number): Draft | undefined => {
    const key = draftKey(targets[i]!.project_id, null);
    const matches = (d: Draft) => d.review?.origin?.chat === chat.task_id
      && d.review.origin.run === run.run_id && d.review.origin.index === indices[i];
    const active = drafts[key];
    return active && matches(active) ? active : [...(savedDrafts[key] ?? [])].reverse().find(matches);
  };
  // Each task's usual agent for its kind; what the person picks here wins.
  const usual = useMemo(() => suggestions.map((_, i) => usualAgent(profile.profileId, targets[i], kinds[i]!, harnesses)?.agent ?? null),
    [suggestions, targets, kinds, harnesses, profile.profileId]);
  const [setPick, setSetPick] = useState<AgentChoice | null>(null);
  const [rowPicks, setRowPicks] = useState<Record<number, AgentChoice>>({});
  const allDesktop = kinds.every((k) => k === "desktop");
  const agentFor = (i: number): AgentChoice | null =>
    rowPicks[i] ?? (setPick && (kinds[i] !== "desktop" || allDesktop) ? setPick : null) ?? usual[i] ?? null;
  const agents = suggestions.map((_, i) => agentFor(i));

  const [local, setLocal] = useState<Record<number, Local>>({});
  // Started here, then put away by the computer before it started: not started.
  const stateOf = (i: number): Local | null => {
    const l = local[i];
    if (l && !(l.phase === "started" && l.taskId && l.taskId in away)) return l;
    return started[i] ? { phase: "started", taskId: started[i]! } : null;
  };
  const [open, setOpen] = useState(false);
  const [picking, setPicking] = useState<Picking>(null);
  // The computer knows each suggestion by its place in the turn's list.
  const plans: CardPlan[] = suggestions.map((w, k) => planCard({
    w, index: indices[k]!, chatId: chat.task_id, runId: run.run_id, chat: project, projects,
    habits: agents[k] ? { agent: agents[k]!, pr } : null, harnesses,
    github: githubView, reviewer, writer, paused, features, canStart: online && mayStart, signInOn,
    // Past a step that didn't succeed, each starts on its own.
    ...(ordered && stopped === null ? { inSet: true, follows: k > 0 ? suggestions[k - 1]!.title : null } : {}),
  })).map((p, i) => p.start && draftFor(i)
    ? { ...p, start: null, review: "Your changes are kept in Review. Continue there before creating this task." }
    : p);

  const set = (i: number, v: Local | null) => setLocal((l) => {
    const next = { ...l };
    if (v) next[i] = v; else delete next[i];
    return next;
  });

  // Two taps can land before the card re-renders as Starting: one start each.
  const inFlight = useRef(new Set<number>());
  const start = async (i: number, fresh = false): Promise<boolean> => {
    const plan = plans[i]!;
    if (!plan.start) { review(i); return false; }
    if (inFlight.current.has(i) || (!fresh && stateOf(i))) return false;
    inFlight.current.add(i);
    set(i, { phase: "starting" });
    try {
      const made = await commandResult(session, await session.command(plan.start, { label: `Start “${suggestions[i]!.title}”` }));
      set(i, { phase: "started", taskId: made?.kind === "task_created" ? made.task_id : null });
      return true;
    } catch (e) {
      set(i, null);
      toast(describeError(e, profile.name), { tone: "stop" });
      return false;
    } finally {
      inFlight.current.delete(i);
    }
  };

  // A set starts in its order, each once the one before is in Next up; one
  // refused stops the rest, which stay on the card to start again.
  const [startingAll, setStartingAll] = useState(false);
  const startingSet = useRef(false);
  const startAll = async () => {
    if (startingSet.current || inFlight.current.size) return;
    startingSet.current = true;
    setStartingAll(true);
    const todo = suggestions.map((_, k) => k).filter((k) => !stateOf(k));
    // Every row flips at once; each settles as the computer takes it.
    setLocal((l) => {
      const next = { ...l };
      for (const k of todo) next[k] = { phase: "starting" };
      return next;
    });
    try {
      for (const k of todo) {
        if (await start(k, true)) continue;
        setLocal((l) => {
          const next = { ...l };
          for (const j of todo) if (j > k && next[j]?.phase === "starting") delete next[j];
          return next;
        });
        break;
      }
    } finally {
      startingSet.current = false;
      setStartingAll(false);
    }
  };

  /** Opens it in Review, or the desktop's own review, with everything filled in. */
  const review = (i: number) => {
    if (startingSet.current || inFlight.current.size || stateOf(i)) return;
    // A set must keep its origin so the computer can bind every dependency.
    if (ordered && stopped === null && (!features.includes(REVIEWED_FEATURE) || !features.includes(SETS_FEATURE))) return;
    const w = suggestions[i]!;
    const plan = plans[i]!;
    const target = targets[i]!;
    const agent = agentFor(i);
    // Started there, it is still this card's task, on a computer that keeps it so.
    const origin: TaskOrigin | undefined = features.includes(REVIEWED_FEATURE) ? { chat: chat.task_id, run: run.run_id, index: indices[i]!, reviewed: true } : undefined;
    setPicking(null);
    try {
      if (plan.kind === "desktop") {
        const key = goalKey(profile.profileId, target.project_id);
        const kept = JSON.parse(localStorage.getItem(key) ?? "null") as { attempt?: unknown } | null;
        // A start still waiting to be confirmed there keeps its own goal.
        if (!kept?.attempt) localStorage.setItem(key, JSON.stringify({ goal: w.brief, target: "", harness: agent?.harness ?? "", model: agent?.model ?? "", ...(origin ? { origin } : {}) }));
        ui.push({ screen: "computer_use_review", projectId: target.project_id }, "control");
        return;
      }
      const kept = draftFor(i);
      if (kept) {
        const key = draftKey(target.project_id, null);
        const active = ui.draft(target.project_id, null);
        if (active !== kept) {
          if (active?.attempt) throw new Error("This project’s new-task draft is waiting to hear about its last send. Open it in Chat first.");
          ui.set((s) => ({ ...s,
            drafts: { ...s.drafts, [key]: kept },
            savedDrafts: { ...s.savedDrafts, [key]: [
              ...(s.savedDrafts[key] ?? []).filter((d) => d !== kept),
              ...(active ? [active] : []),
            ] },
          }));
        }
        if (!ui.persistNow()) throw new Error("The draft couldn’t be saved on this device.");
        ui.push({ screen: "review", projectId: target.project_id, taskId: null, intent: "new_task" }, "talk");
        return;
      }
      const mode = kindMode(plan.kind, w);
      draftNewTask(ui, target.project_id, {
        text: w.brief, title: w.title, mode,
        proposalGithub: w.issue != null ? { type: "issue", number: w.issue }
          : mode === "work" && pr && target.vcs?.kind === "git" ? { type: "new_pr" } : undefined,
        agent: agent ?? null,
        // A question starts there with what its card said it may reach.
        access: leastAccess(w, places(w, project, projects)),
        origin,
        ...(ordered && stopped === null ? {
          ordered: { step: i + 1, total: suggestions.length, after: i > 0 ? suggestions[i - 1]!.title : null },
          paused: !!session.state.get().queues[target.project_id]?.paused,
        } : {}),
      });
    } catch (e) {
      toast(describeError(e, profile.name), { tone: "stop" });
    }
  };

  /** The chat's own agent does it here: it may change the project first, once the computer says so. */
  const here = async (i: number) => {
    const w = suggestions[i]!;
    const plan = plans[i]!;
    setPicking(null);
    set(i, { phase: "here" });
    const before = chat.access;
    let widened = false;
    try {
      const changes = plan.kind === "change" || plan.kind === "fix";
      if (changes && held && before && !before.whole_write && !before.write.includes(pid)) {
        const wider = normalizedAccess({ ...before, write: [...before.write, pid] });
        await commandResult(session, await session.command({ command: "set_conversation_access", task_id: chat.task_id, access: wider }, { label: "Let the agent change the project" }), { waitMs: 45_000 });
        widened = true;
      }
      // Changing what the chat may reach is a change to the chat: the message
      // goes to the chat as it is now.
      const revision = session.state.get().tasks[chat.task_id]?.revision ?? chat.revision;
      const text = `Do this here in our conversation, not as a separate task.\n\n${w.title}\n\n${w.brief}`;
      await commandResult(session, await session.command({ command: "resume_task", task_id: chat.task_id, text }, { label: "Do it here", expectedRevision: revision }));
    } catch (e) {
      set(i, null);
      if (!widened || !before) {
        toast(describeError(e, profile.name), { tone: "stop" });
        return;
      }
      // Nothing started here, so the chat goes back to what it could reach.
      try {
        await commandResult(session, await session.command({ command: "set_conversation_access", task_id: chat.task_id, access: before }, { label: "Set the chat back" }), { waitMs: 45_000 });
        toast(`${describeError(e, profile.name)} Nothing started here, and the chat can no longer change ${project.name}.`, { tone: "stop" });
      } catch {
        toast(`${describeError(e, profile.name)} The chat may still change ${project.name}: set it back on its access line.`, { tone: "stop" });
      }
    }
  };

  // A pick is the person's own choice: remembered for that kind of task,
  // and for a whole set only when every task in it is the same kind.
  const pickedRow = useCallback((i: number, a: AgentChoice) => {
    setRowPicks((p) => ({ ...p, [i]: a }));
    rememberUsual(profile.profileId, targets[i]!.project_id, kinds[i]!, a);
  }, [profile.profileId, targets, kinds]);
  const pickedSet = useCallback((a: AgentChoice) => {
    setSetPick(a);
    setRowPicks({});
    const one = kinds.every((k) => k === kinds[0]);
    if (one) rememberUsual(profile.profileId, pid, kinds[0]!, a);
  }, [profile.profileId, pid, kinds]);

  // Only while one of its turns is going; a chat's last turn stays its current run.
  const busy = useHost((s) => isTaskRunning(s, chat));
  // Here is this chat, in its own project, reaching what it already may: a
  // change only adds its own project to change, after the computer confirms.
  const canHere = (i: number) => {
    const plan = plans[i]!;
    // A set's task done here would leave the ones after it nothing to follow.
    if (ordered) return false;
    if (!online || !mayStart || busy || plan.refused || plan.kind === "desktop") return false;
    if (targets[i]!.project_id !== pid) return false;
    if (plan.kind === "find") {
      const need = leastAccess(suggestions[i]!, places(suggestions[i]!, project, projects));
      return !held || !need || covers(heldAccess(chat, held), need);
    }
    return plan.kind === "plan" || held;
  };
  const one = suggestions.length === 1 && !ordered;
  const more = plans.filter(asksMore).length;
  const rest = suggestions.map((_, i) => i).filter((i) => !stateOf(i));
  // What keeps Start all from starting the rest: a step that didn't succeed,
  // or the first reason one still to start can't start from here.
  const blocked = rest.map((i) => plans[i]!).find((p) => !p.start);
  const stopWhy = stopped === null ? null
    : `“${suggestions[stopped]!.title.trim()}” ${started[stopped] ? "didn’t succeed" : "didn’t start"}, so the rest don’t start after it. ${open ? "Start" : "Open the list to start"} any of them on its own if you still want it.`;
  const restWhy = stopWhy ?? (!blocked ? null : blocked.refused ? "One of them can’t start as written. Open the list to see which." : blocked.review);
  const setSupported = features.includes(SETS_FEATURE);
  const reviewSet = ordered && stopped === null && !!blocked;
  const canReviewSet = setSupported && features.includes(REVIEWED_FEATURE);
  const setWhy = !setSupported ? "Update Øphio on this computer to create these tasks in order."
    : reviewSet && !canReviewSet ? "Update Øphio on this computer to keep these tasks in order through Review." : restWhy;

  // The set's chip: the one choice they share, or each task's own usual one.
  const live = agents.filter((a, i) => a && !plans[i]!.refused && !stateOf(i)) as AgentChoice[];
  const shared = live.length > 0 && live.every((a) => sameChoice(a, live[0]!)) ? agentLabel(harnesses, live[0]!) : null;
  // An ordered set in order says once what its tasks to start share.
  const inOrder = ordered && stopped === null;
  const waiting = rest.filter((i) => !plans[i]!.refused);
  const what = ordered && waiting.length > 0 && waiting.every((i) => plans[i]!.what === plans[waiting[0]!]!.what) ? plans[waiting[0]!]!.what : null;
  // Each step says only what differs: in order nothing; past a failed step,
  // when it would start, if not now. The reason it was put away is the set's.
  const lineOf = (i: number): string | null => {
    const plan = plans[i]!;
    if (plan.refused || what === null) return plan.refused ?? plan.line;
    const when = plan.line.slice(plan.what.length + 3);
    return inOrder || !when || when === "starts now" ? null : when;
  };
  const rows = suggestions.map((w, i) => (
    <Row key={i} w={w} plan={plans[i]!} local={stateOf(i)} lead={i === 0} n={ordered ? i + 1 : null}
      own={!inOrder} line={lineOf(i)} chip={!(ordered && shared !== null)}
      onChip={() => setPicking({ scope: "row", i })}
      onStart={() => void start(i)} onReview={() => review(i)} />
  ));

  const pickerFor = picking?.scope === "row" ? picking.i : null;
  const pickerKind: CardKind | null = pickerFor !== null ? kinds[pickerFor]! : allDesktop ? "desktop" : null;
  const pickerValue = pickerFor !== null ? agentFor(pickerFor) : setPick ?? live[0] ?? agents.find(Boolean) ?? null;
  const pickerProject = pickerFor !== null ? targets[pickerFor]! : project;
  // A set's task opened in Review alone would start outside the set.
  const after = pickerFor !== null && !ordered && !plans[pickerFor]!.refused ? (
    <div className="group work-card__more">
      <ListRow icon={SlidersHorizontal} title="More options" subtitle={plans[pickerFor]!.kind === "desktop" ? "Choose the screen and the rest" : "Mode, permission prompts and the rest, in Review"} chevron onClick={() => review(pickerFor)} />
      {canHere(pickerFor) && <ListRow icon={MessageSquare} title="Do it here instead" subtitle="This chat’s agent does it, in this chat" chevron onClick={() => void here(pickerFor)} />}
    </div>
  ) : null;

  return (
    <section className="work-cards" aria-labelledby={heading}>
      {one ? <>
        <h3 id={heading} className="visually-hidden">Suggested task</h3>
        {rows}
      </> : <>
        <button type="button" className="work-cards__opener" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
          <span className="work-cards__count" id={heading}>
            {ordered && <ListOrdered aria-hidden className="work-cards__order" strokeWidth={2.1} />}
            {ordered ? `${suggestions.length} tasks, in order` : `${suggestions.length} suggested tasks`}
          </span>
          <span className="work-cards__titles">{suggestions.map((w) => w.title).join(" · ")}</span>
          <ChevronDown aria-hidden className="work-cards__chevron" strokeWidth={2.1} />
        </button>
        {/* What any of them would reach past the project shows while folded too. */}
        {!open && more > 0 && <p className="work-card__beyond">{more === 1 ? "One asks for more than usual. Open to see what." : `${more} ask for more than usual. Open to see what.`}</p>}
        {/* Opened, tasks that stand alone each show the agent they share, so
            the set's own chip would only say it once more. */}
        {live.length > 0 && !(open && !ordered && shared !== null) && <AgentChip label={shared ?? "Each task’s usual agent"} lead="Agent for these tasks:" onClick={() => setPicking({ scope: "set" })} />}
        {open && what && <p className="work-cards__line">{inOrder ? `${what} · each starts after the one before` : what}</p>}
        {ordered && rest.length > 0 && <>
          {setWhy && <p className="work-card-item__why">{setWhy}</p>}
          {stopped === null && (reviewSet
            ? <>
              <Button className="work-cards__all" size="sm" variant="primary" icon={SlidersHorizontal}
                disabled={!canReviewSet || startingAll} onClick={() => review(rest[0]!)}>
                {rest.length < suggestions.length ? "Review next task" : "Review tasks"}
              </Button>
              {canReviewSet && <p className="work-card-item__why">Review creates one task at a time. Return here to start or review the rest, in order.</p>}
            </>
            : <Button className="work-cards__all" size="sm" variant="primary" icon={Play} disabled={startingAll || !setSupported} onClick={() => void startAll()}>
              {rest.length < suggestions.length ? "Start the rest" : "Start all"}
            </Button>)}
        </>}
        {open && (ordered
          ? <ol className="work-cards__list work-cards__list--ordered">{rows.map((r, i) => <li key={i}>{r}</li>)}</ol>
          : <div className="work-cards__list">{rows}</div>)}
      </>}
      {picking && pickerValue && (
        <AgentSheet
          open
          compact
          onClose={() => setPicking(null)}
          project={pickerProject}
          value={pickerValue}
          onChange={(a) => (pickerFor !== null ? pickedRow(pickerFor, a) : pickedSet(a))}
          mode={pickerKind ? kindMode(pickerKind, suggestions[pickerFor ?? 0]!) : "work"}
          launchFlags={false}
          computerUse={pickerKind === "desktop"}
          title={pickerFor !== null ? "Agent for this task" : `Agent for these ${suggestions.length} tasks`}
          doneLabel="Done"
          note={pickerFor !== null ? undefined : kinds.every((k) => k === kinds[0]) ? undefined : "Applies to each of them. It isn’t remembered, because they are different kinds of task."}
          after={after}
        />
      )}
    </section>
  );
}

const NONE: readonly string[] = [];

/** Put away by the computer before it ever started. */
const putAway = (t: Task) => t.state === "archived" && !t.current_run;

/** Mirrors the computer's proposals::succeeded: failed, stopped short, or put away is not. */
function succeeded(t: Task, runs: Record<string, Run>): boolean {
  if (t.state === "failed" || t.state === "interrupted") return false;
  if (t.state !== "archived") return true;
  const r = t.current_run ? runs[t.current_run] : undefined;
  return !r || (r.state !== "failed" && r.state !== "interrupted");
}

/** A fix the computer started as a plan, as it does any issue from outside
 * the project: said, since the card offered a fix. */
export function planned(task: Task): string | null {
  return task.mode === "plan" && task.github?.from_stranger && task.github.link.type === "issue"
    ? "Started as a plan, because the issue comes from outside the project."
    : null;
}

function Row({ w, plan, local, lead, n, own, line, chip, onChip, onStart, onReview }: {
  w: Suggestion; plan: CardPlan; local: Local | null; lead: boolean;
  /** Its place in an ordered set, which starts as a whole. */
  n: number | null;
  /** Whether it has a Start of its own: alone, or in a set stopped by a step that didn't succeed. */
  own: boolean;
  /** Its line, or only what differs from what its set says once. */
  line: string | null;
  /** Whether it shows its own agent chip, rather than the set's. */
  chip: boolean;
  onChip: () => void; onStart: () => void; onReview: () => void;
}) {
  const startedId = local?.phase === "started" ? local.taskId : null;
  const startedTask = useHost((s) => (startedId ? s.tasks[startedId] : undefined));
  // Once the computer has the task, the row is that task, as Tasks lists it.
  if (startedTask) return <LiveRow task={startedTask} />;
  return (
    <div className={`work-card-item${local ? ` is-${local.phase}` : ""}${chip ? "" : " work-card-item--setchip"}`}>
      <div className="work-card-item__text">
        <h4 className="work-card-item__title">{w.title}</h4>
        {line && <p className="work-card-item__line">{line}</p>}
        {!plan.refused && plan.beyond.length > 0 && <p className="work-card__beyond">{`More than usual: ${new Intl.ListFormat("en", { type: "conjunction" }).format(plan.beyond)}.`}</p>}
        {!local && !plan.refused && plan.review && <p className="work-card-item__why">{plan.review}</p>}
      </div>
      {local?.phase === "starting" && <Pill tone="accent">Starting</Pill>}
      {local?.phase === "here" && <Pill tone="accent">Asked here</Pill>}
      {local?.phase === "started" && <Pill tone="ok">{plan.queued ? "In Next up" : "Started"}</Pill>}
      {!local && (plan.refused
        ? (n === null || own) && <Button size="sm" variant="secondary" onClick={onReview}>Open in Review</Button>
        : (chip || n === null || own) && <div className="work-card-item__go">
          {chip && <AgentChip label={plan.chip} lead="Agent for this task:" onClick={onChip} />}
          {/* A set's tasks start together, in order, from Start all. */}
          {(n === null || own) && <Button size="sm" variant={lead ? "primary" : "secondary"} icon={Play} onClick={onStart}>{plan.start ? "Start" : "Review"}</Button>}
        </div>)}
    </div>
  );
}

/** A started task on its card: its name and how it stands, updating in
 * place without a sound; tapping it opens the task, and Back comes here. */
function LiveRow({ task }: { task: Task }) {
  const open = useOpenTask();
  const live = useHost((s) => cardLive(s, task), shallowEqual);
  // Started on the project's go-ahead rather than a tap, it says so, and
  // Stop is on the task this opens. A fix started as a plan says why.
  const line = [planned(task), task.origin?.auto ? "Started without asking." : null, live.why].filter(Boolean).join(" ");
  return (
    <button type="button" className="work-card-item work-card-item--live is-started" onClick={() => open(task)}>
      <span className="work-card-item__text">
        <span className="work-card-item__title">{task.title}</span>
        {line && <span className="work-card-item__line">{line}</span>}
      </span>
      <Pill tone={live.tone}>{live.label}</Pill>
      <ChevronRight className="work-card-item__chev" aria-hidden />
    </button>
  );
}
