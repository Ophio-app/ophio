// SPDX-License-Identifier: Apache-2.0
import { ChevronRight, Folder, Play, Workflow } from "lucide-react";
import { useMemo, useState } from "react";
import { shallowEqual, useApp, useCommand, useHost, useUi } from "../../app/context";
import { Button } from "../../design/Button";
import { Empty, ListRow } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Screen, useRoute } from "../../shell/Screen";
import { isActiveTask, openAttention, projectTasks, type HostState } from "../../state/projection";
import { useStore } from "../../state/store";
import type { QueueView, Run, Task } from "../../protocol/types";
import type { RouteProps } from "../types";
import { hasEndedTask } from "./logic";
import { needsYouFirst, taskAttentionReasons } from "./taskActivity";
import { projectAttention } from "./attention";
import { buildBrief, digestSentences, stopChecks } from "./brief";
import { inboxStore } from "./inbox";
import { useProjectPages, useProjectShown } from "./useProjectShown";
import { useVisitCutoff } from "./local";
import ProjectSheet from "./ProjectSheet";
import { CommandFeedback, ConnectionNote, listedAttention, useCanWork, useOpenTask } from "./shared";
import { writerHold, writerHoldText } from "../talk/intent";
import { pendingQueueItems, queueItemState, queueState, useQueue } from "./useQueue";
import "./work.css";
import { ContextHeader } from "../../shell/ContextHeader";
import { ProjectSheet as Projects } from "../../shell/ProjectSheet";
import { WorkRoot, type WorkView } from "./WorkRoot";
import { WaitingHandoffs } from "../contribute/Waiting";

/** The runs of `tasks`, in the order the runs record lists them, found again
 * only when the runs or the tasks change rather than on every store update. */
function runsOf(tasks: Task[]): (s: HostState) => Run[] {
  const ids = new Set(tasks.map((t) => t.task_id));
  let found: { from: HostState["runs"]; runs: Run[] } | null = null;
  return (s) => {
    if (found?.from !== s.runs) found = { from: s.runs, runs: Object.values(s.runs).filter((r) => ids.has(r.task_id)) };
    return found.runs;
  };
}

export default function Work(_: RouteProps<"work">) {
  const id = useUi((s) => s.projectId);
  return <ProjectWork key={id ?? "none"} projectId={id} />;
}
function ProjectWork({ projectId }: { projectId: string | null }) {
  const { profile, session, ui } = useApp();
  const { active } = useRoute();
  const projects = useHost((s) => Object.values(s.projects), shallowEqual);
  const project = projects.find((p) => p.project_id === projectId);
  const tasks = useHost(
    (s) =>
      projectId
        ? projectTasks(s, projectId).filter((t) => t.state !== "archived" && !t.conversation)
        : [],
    shallowEqual,
  );
  // Archived conversations are Talk's to show.
  const archived = useHost(
    (s) => (projectId ? projectTasks(s, projectId).filter((t) => t.state === "archived" && !t.conversation) : []),
    shallowEqual,
  );
  const runs = useHost(useMemo(() => runsOf(tasks), [tasks]), shallowEqual);
  const attention = useHost((s) => openAttention(s).filter(a => a.task_id && s.tasks[a.task_id]?.project_id === projectId), shallowEqual);
  const acrossProjects = useHost(projectAttention, (a, b) => JSON.stringify(a) === JSON.stringify(b));
  const heard = useHost((s) => !!s.lastHeardAt);
  const gap = useHost((s) => s.gaps.length > 0);
  const [settings, setSettings] = useState(false);
  const [switcher, setSwitcher] = useState(false);
  const key = `${profile.profileId}.seen.${projectId}`;
  const since = useVisitCutoff(key, active && !!projectId, () => session.state.get().lastHeardAt);
  const { queue } = useQueue(projectId);
  const brief = buildBrief(projectId ?? "", since, tasks, runs, attention, Infinity, queue);
  const stopped = useHost((s) => stopChecks(s, attention), shallowEqual);
  // What needs the person is listed on its own, so the brief doesn't count it.
  const digest = digestSentences(brief, 0, stopped);
  const inboxCount = useStore(
    inboxStore(profile.profileId),
    (s) => s.items.length,
  );
  // What agents showed waits in the inbox beside what was shared here.
  const shown = useProjectShown(projectId);
  const pages = useProjectPages(projectId);
  const shownCount = shown.media.length + pages.length;
  const reasons = useHost((s) => taskAttentionReasons(s, tasks), shallowEqual);
  // Each task shows in one place: one waiting on the person is under Needs
  // you alone, not Running, To check or Next up as well.
  const needsYou = useHost((s) => projectId ? [...new Set(listedAttention(s, projectId).flatMap((a) => a.task_id ?? []))].sort() : [], shallowEqual);
  const going = tasks.filter(
    (t) =>
      isActiveTask(t) &&
      !queue?.items.some(
        (i) => i.task_id === t.task_id && i.state.state !== "started",
      ),
  );
  const now = needsYouFirst(going.filter((t) => !needsYou.includes(t.task_id)), reasons);
  const older = useHost((s) => !!projectId && projectId in s.olderTasks);
  const queued = !!project && pendingQueueItems(queue).length > 0;
  // A run that ended without a known result keeps the project's writer, so
  // nothing is running and yet Work tasks can't start. Say which task it is.
  const held = useHost(
    (s) => writerHold(projectId ? s.projects[projectId]?.active_writer : null, s),
    shallowEqual,
  );
  const openTask = useOpenTask();
  const heldNotice = project && held?.unresolved && (
    <Notice
      tone="warn"
      title="Waiting for you to check"
      action={
        held.task && (
          <Button size="sm" variant="secondary" trailingIcon={ChevronRight} onClick={() => openTask(held.task!)}>
            Check it
          </Button>
        )
      }
    >
      {writerHoldText(held, project.name)}
    </Notice>
  );
  const firstTask = project && !tasks.length && (
    <Empty icon={Folder} title="A clear place to begin">
      Tasks and their results will appear here. A new task stays a draft
      until you review it.
    </Empty>
  );
  const noProject = (
    <Empty
      icon={Folder}
      title={heard ? "Choose a project" : "Waiting for the computer"}
      action={
        projects.length ? (
          <Button onClick={() => setSwitcher(true)}>Choose project</Button>
        ) : (
          <Button onClick={() => setSettings(true)}>Add project</Button>
        )
      }
    >
      Projects are folders you choose on the computer.
    </Empty>
  );
  const view: WorkView | null = project ? {
    project,
    any: tasks.length > 0 || archived.length > 0 || older,
    older,
    running: now,
    runningNeedsYou: going.length - now.length,
    open: tasks.filter((t) => !hasEndedTask(t)).length,
    // Ended without a known result: the one holding the project has its notice.
    unsettled: tasks.filter((t) => t.state === "reconciliation_required" && t.task_id !== held?.task?.task_id && !needsYou.includes(t.task_id)),
    finished: tasks.filter(hasEndedTask),
    archived,
    queued,
    nextUp: queued ? <QueuedWork queue={queue!} needsYou={needsYou} /> : <NothingNextUp queue={queue} />,
    nextUpNote: queue ? `${queue.paused ? "Paused · " : ""}Nothing waiting` : "Queue not loaded",
    held: heldNotice || null,
    brief: { lead: digest.lead, more: [...new Set([...digest.details, ...brief.unknowns])].filter((s) => s !== digest.lead), since, gap },
    inboxCount,
    shownCount,
    others: acrossProjects.filter((group) => group.project.project_id !== projectId),
    onSettings: () => setSettings(true),
    onSelect: (id) => ui.selectProject(id),
    empty: firstTask,
  } : null;
  return (
    <Screen header={<ContextHeader heading />} className="work-screen work-root">
      <ConnectionNote />
      {view ? <WorkRoot v={view} /> : <><WaitingHandoffs />{noProject}</>}
      {settings && <ProjectSheet project={project} onClose={() => setSettings(false)} />}
      <Projects open={switcher} onClose={() => setSwitcher(false)} />
    </Screen>
  );
}

/** Next up while nothing waits in it, and how something comes to. */
function NothingNextUp({ queue }: { queue: QueueView | null | undefined }) {
  const { ui } = useApp();
  return (
    <div className="group">
      <ListRow
        icon={Workflow}
        title={queue ? "Nothing waiting" : "Queue not loaded"}
        ariaLabel={`Open Next up: ${queue ? "Nothing waiting" : "Queue not loaded"}`}
        subtitle={queue ? `${queue.paused ? "Paused · " : ""}When you review a task, Add to Next up lines it up here` : "What is waiting shows here once the computer sends it"}
        chevron
        onClick={() => ui.push({ screen: "next_up" })}
      />
    </div>
  );
}

/**
 * The first task waiting in Next up, since a paused queue starts nothing and
 * is easy to forget. One action: Resume while paused, otherwise open Next up.
 */
function QueuedWork({ queue, needsYou }: { queue: QueueView; needsYou: string[] }) {
  const { ui } = useApp();
  const items = pendingQueueItems(queue);
  // One waiting on the person, as for its go-ahead, is under Needs you.
  const index = items.findIndex((i) => !needsYou.includes(i.task_id));
  const front = items[Math.max(index, 0)]!;
  const taskTitle = useHost((s) => s.tasks[front.task_id]?.title);
  const title = index < 0 ? "Under Needs you" : taskTitle ?? "Task details not reported";
  const hold = useHost((s) => (s.tasks[front.task_id]?.mode ?? "work") === "work" ? writerHold(queue.writer, s) : null, shallowEqual);
  const can = useCanWork();
  const send = useCommand();
  const [pending, setPending] = useState(false);
  const state = queueState(queue);
  const resume = async () => {
    setPending(true);
    await send({ command: "queue_resume", project_id: queue.project_id }, { label: "Resume queue", expectedRevision: queue.revision, toastErrors: false });
    setPending(false);
  };
  const row = (
    <ListRow
      icon={Workflow}
      title={title}
      ariaLabel={`Open Next up: ${taskTitle || index < 0 ? title : "first task"}`}
      subtitle={`${state} · ${items.length} waiting${index < 0 ? "" : ` · ${queueItemState(front, index, hold)}`}`}
      chevron
      onClick={() => ui.push({ screen: "next_up" })}
    />
  );
  const paused = queue.paused && (
    <>
      <p className="work-muted">{queue.pause_reason ?? "Next up is paused. Resume it to let tasks start."}</p>
      <Button variant="secondary" icon={Play} disabled={!can || pending} pending={pending} onClick={() => void resume()}>
        Resume queue
      </Button>
      <CommandFeedback limit={1} match={(c) => c.command === "queue_resume" && c.project_id === queue.project_id} />
    </>
  );
  return <div className="group">{row}{paused && <div className="work-paused">{paused}</div>}</div>;
}
