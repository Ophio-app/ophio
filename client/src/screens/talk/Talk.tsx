// SPDX-License-Identifier: Apache-2.0
import { prefs } from "../../app/prefs";
import { useStore } from "../../state/store";
//
// Talk: one project's conversations, one at a time, as a chat (D-065). The
// header says what the agent is doing, the transcript shows what was said
// and done, and the composer offers only what the computer can honestly do
// next. Back from a conversation shows the project's conversations, as a
// messaging app's chats, with the tabs. A task opens here too, from Work,
// and Back from it goes to Work.

import { ArrowDown, Bot, FolderPlus, MessageSquarePlus, Sparkles } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { shallowEqual, useApp, useCommand, useHost, useLink, useUi } from "../../app/context";
import { talkProject, talkTask, usePinnedTalkTask } from "../../app/taskOnScreen";
import { draftKey, type AgentChoice } from "../../app/ui";
import { useStickToBottom } from "./stickToBottom";
import { ReadinessCheck } from "../work/ReadinessCheck";
import { Button } from "../../design/Button";
import { Empty } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import type { CatalogKind, Project, Run, Task } from "../../protocol/types";
import { projectTasks, replyAttention, reviewAttention } from "../../state/projection";
import { Screen, useRoute } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { HandBack } from "./HandBack";
import { useQueue } from "../work/useQueue";
import { ManualAddProject } from "../work/ProjectSheet";
import { ConnectionNote } from "../work/shared";
import { Composer } from "./Composer";
import { CONVERSATIONS_FEATURE, NEW_CONVERSATION, startConversation, startWith } from "./conversation";
import { accessHeld, askAccess, defaultAccess, describeAccess, unheldWords } from "./access";
import { send } from "./send";
import { LAUNCHERS_FEATURE, type LaunchStart } from "../../app/launchers";
import { StartFromSheet } from "../launch/StartFrom";
import { announce } from "../../design/Announcer";
import { TalkConversations } from "./Conversations";
import { canStart, resolveAgent } from "./intent";
import { TalkHeader, TalkPins } from "./TalkHeader";
import { latestRun } from "../work/logic";
import { runOutcome } from "../work/outcome";
import { OutcomeCard } from "../work/OutcomeCard";
import { useCarryPlan } from "./carryPlan";
import { Transcript } from "./Transcript";
import { PathText } from "../../design/PathText";
import { AlertsOffer } from "../../shell/AlertsOffer";
import { useViewingTask } from "../../app/viewing";

export default function Talk(_: RouteProps<"talk">) {
  const caughtUp = useHost((s) => s.caughtUp);
  const chosen = useUi((s) => s.projectId);
  const project = useHost((s) => talkProject(s, chosen));
  const list = useUi((s) => s.taskList);
  const conversing = useUi((s) => s.conversing);
  const pid = project?.project_id;
  const selected = useUi((s) => (pid ? s.taskByProject[pid] : undefined));
  // With nothing chosen, Talk opens on a conversation still replying, else on the list.
  const replying = useHost((s) => !!pid && selected === undefined && !!talkTask(s, pid, undefined));

  if (!project) {
    return <Screen>{caughtUp ? <NoProjects /> : <div className="talk-loading" aria-busy="true" aria-label="Loading projects"><span /><span /><span /></div>}</Screen>;
  }
  if (list || (selected === undefined && !replying && !conversing)) return <TalkConversations key={project.project_id} project={project} />;
  return <TalkProject key={project.project_id} project={project} />;
}

function TalkProject({ project }: { project: Project }) {
  const pid = project.project_id;
  const selected = useUi((s) => s.taskByProject[pid]);
  const tasks = useHost((s) => projectTasks(s, pid), shallowEqual);
  const caughtUp = useHost((s) => s.caughtUp);
  const harnesses = useHost((s) => s.harnesses);

  const task = usePinnedTalkTask(pid);
  useQueue(tasks.some((t) => t.state === "accepted") ? pid : null);
  const missing = typeof selected === "string" && !task;
  const arriving = useArrival(missing ? selected : null);
  const run = useHost((s) => task ? latestRun(task, s.runs) ?? null : null);
  const conversing = useUi((s) => s.conversing) && !task;
  const chosenAgent = useUi((s) => s.drafts[draftKey(pid, conversing ? NEW_CONVERSATION : null)]?.agent ?? null);
  useStore(prefs, p => p);
  const agent = task ? null : resolveAgent(project, harnesses, chosenAgent);
  const harness = harnesses.find((h) => h.kind === (task ? task.harness : agent?.harness)) ?? null;

  const { ref, away, jump, sent } = useStickToBottom(task?.task_id ?? null);
  useRepliesRead(task);

  if (missing && (!caughtUp || arriving)) {
    return (
      <Screen>
        <div className="talk-loading" aria-busy="true" aria-label="Loading the task">
          <span />
          <span />
          <span />
        </div>
      </Screen>
    );
  }

  return (
    <Screen
      className="talk"
      bodyRef={ref}
      header={<TalkHeader project={project} task={task ?? undefined} run={run ?? undefined} />}
      top={<TalkPins task={task ?? undefined} project={project} />}
      footer={
        <div className="talk-foot">
          {task?.conversation && task.state === "archived"
            ? <ArchivedConversation project={project} />
            : <Composer project={project} task={task} run={run} harness={harness} agent={agent} conversation={conversing} onSent={sent} />}
        </div>
      }
    >
      {task
        ? <Transcript task={task} agent={harness?.display_name ?? "The agent"} />
        : conversing
          ? <NewConversation project={project} agent={agent} hasAgent={harnesses.some(canStart)} missing={missing} />
          : <NewTask project={project} hasAgent={harnesses.some(canStart)} missing={missing} />}
      {task && run && <TalkOutcome project={project} task={task} run={run} />}
      {task && <HandBack task={task} />}
      <AlertsOffer projectId={pid} taskId={task?.task_id ?? null} />
      {away && task && (
        <div className="talk-jump">
          <button type="button" className="talk-jump__pill" onClick={jump}>
            <ArrowDown aria-hidden strokeWidth={2.2} />
            <span className="talk-jump__label">Latest</span>
          </button>
        </div>
      )}
    </Screen>
  );
}

/**
 * What the latest turn came to, under it: what the agent asks or left, new
 * work it found, and archiving once it ended, as on the receipt. Carry on
 * and Try again go to the task at once; answering goes in the message
 * below, and a turn waiting for the person already asks in the conversation.
 * A conversation's reply is just its next message, so only a turn that
 * failed or was stopped says so.
 */
function TalkOutcome({ project, task, run }: { project: Project; task: Task; run: Run }) {
  const plan = useCarryPlan(task);
  const outcome = runOutcome(task, run);
  if (!outcome || outcome.kind === "waiting" || task.state === "archived" || task.state === "reconciliation_required") return null;
  if (task.conversation && outcome.kind !== "failed" && outcome.kind !== "stopped") return null;
  return (
    <div className="talk-outcome">
      <OutcomeCard task={task} run={run} project={project} outcome={outcome} plan={plan} writing compact />
    </div>
  );
}

/** Whether a task this device just created is still on its way from the computer. */
function useArrival(taskId: string | null): boolean {
  const { ui } = useApp();
  const [, expire] = useState(0);
  const wait = taskId ? ui.arrivalWait(taskId) : 0;
  useEffect(() => {
    if (!wait) return;
    const t = setTimeout(() => expire((n) => n + 1), wait);
    return () => clearTimeout(t);
  }, [taskId, wait]);
  return wait > 0;
}

/**
 * While a conversation or task is on screen and the computer is connected,
 * its replies and results are read: they leave the conversations list's New
 * reply, or Finished's Unread, as a chat's unread count clears once it is open.
 */
function useRepliesRead(task: Task | null) {
  const { active } = useRoute();
  useViewingTask(task?.task_id);
  const online = useLink().state === "online";
  const send = useCommand();
  const unread = useHost((s) => task ? (task.conversation ? replyAttention(s) : reviewAttention(s)).filter((a) => a.task_id === task.task_id).map((a) => a.attention_id) : [], shallowEqual);
  const sent = useRef(new Set<string>());
  useEffect(() => {
    if (!active || !online) return;
    for (const id of unread) {
      if (sent.current.has(id)) continue;
      sent.current.add(id);
      void send({ command: "acknowledge_attention", attention_id: id }, { label: "Mark reply read", toastErrors: false });
    }
  }, [active, online, unread, send]);
}

/**
 * A new conversation, before its first message: what its agent may reach,
 * and a way to start it from one of the computer's agents or skills.
 */
function NewConversation({ project, agent, hasAgent, missing }: { project: Project; agent: AgentChoice | null; hasAgent: boolean; missing: boolean }) {
  const app = useApp();
  const { profile } = app;
  const pid = project.project_id;
  const held = useHost((s) => accessHeld(s.capabilities));
  const why = useHost((s) => unheldWords(s.capabilities));
  const chosen = useUi((s) => s.drafts[draftKey(pid, NEW_CONVERSATION)]?.access);
  const access = held ? chosen ?? defaultAccess(pid) : askAccess(pid);
  const lede = useHost((s) => describeAccess(access, project, s.projects));
  const launchers = useHost((s) => !!s.capabilities?.features.includes(LAUNCHERS_FEATURE));
  const canWork = useHost((s) => s.grants?.tasks ?? false);
  const harnesses = useHost((s) => s.harnesses);
  const [picking, setPicking] = useState<CatalogKind | null>(null);
  const [open, setOpen] = useState<CatalogKind>("agent");

  const begin = (from: LaunchStart, asked: AgentChoice | null) => {
    setPicking(null);
    // The entry's own agent, model and effort, held to what a conversation may
    // do rather than the flags it would launch a task with.
    const chosenAgent = asked ? resolveAgent(project, harnesses, { ...asked, launchPolicy: undefined }) : agent;
    if (!chosenAgent) return;
    const text = startWith(from.name);
    void send(app, { projectId: pid, draftTaskId: NEW_CONVERSATION, text, annotationId: null, label: text, build: () => startConversation(pid, text, chosenAgent, { start: from.start, access: held ? access : null }) }, () => {}).then((out) => {
      if (!out.ok) announce(out.attempt.detail, "assertive");
    });
  };
  const choose = (kind: CatalogKind) => { setOpen(kind); setPicking(kind); };

  return (
    <div className="talk-new">
      {missing && (
        <Notice tone="info" title="That conversation isn’t on this computer any more">
          It may have been removed on {profile.name}. You can start a new one here.
        </Notice>
      )}
      <div className="talk-new__head">
        <div className="talk-new__identity">
          <p className="talk-new__eyebrow">New conversation</p>
          <h1 className="talk-new__title">{project.name}</h1>
        </div>
        <p className="talk-new__lede">{lede}</p>
        {why && <p className="talk-new__why">{why}</p>}
      </div>
      {hasAgent && launchers && canWork && (
        <div className="talk-new__starts">
          <Button variant="secondary" icon={Bot} onClick={() => choose("agent")}>Start from agent</Button>
          <Button variant="secondary" icon={Sparkles} onClick={() => choose("skill")}>Start from skill</Button>
        </div>
      )}
      {!hasAgent && <NoAgent />}
      {launchers && canWork && (
        <StartFromSheet open={picking !== null} onClose={() => setPicking(null)} projectId={pid} only={open} onChoose={begin} />
      )}
    </div>
  );
}

function NoAgent() {
  const { ui, profile } = useApp();
  return (
    <Notice
      tone="warn"
      title="No agent can start work here yet"
      action={
        <ReadinessCheck>
          <Button size="sm" variant="secondary" onClick={() => ui.push({ screen: "agents" }, "work")}>
            See agents
          </Button>
        </ReadinessCheck>
      }
    >
      {profile.name} didn’t report an agent this device can start. See agents for the missing setup or ability.
    </Notice>
  );
}

function NewTask({ project, hasAgent, missing }: { project: Project; hasAgent: boolean; missing: boolean }) {
  const { profile } = useApp();
  return (
    <div className="talk-new">
      {missing && (
        <Notice tone="info" title="That task isn’t on this computer any more">
          It may have been removed on {profile.name}. You can start a new one here.
        </Notice>
      )}
      <div className="talk-new__head">
        <div className="talk-new__identity">
          <p className="talk-new__eyebrow">New task</p>
          <h1 className="talk-new__title">{project.name}</h1>
          <p className="talk-new__path">
            <PathText path={project.host_path} />
            {project.vcs?.branch ? <span className="talk-new__branch"> · {project.vcs.branch}</span> : null}
          </p>
        </div>
        <p className="talk-new__lede">
          Say what you want done. Before anything starts you’ll see the agent, the folder and what it may do without asking.
        </p>
      </div>
      {!hasAgent && <NoAgent />}
    </div>
  );
}

function NoProjects() {
  const { profile } = useApp();
  return (
    <>
      <ConnectionNote />
      <Empty icon={FolderPlus} title={`Add a project on ${profile.name}`}>
        <div className="talk-addproject">
          <ManualAddProject key={profile.profileId} />
        </div>
      </Empty>
    </>
  );
}

/** An archived conversation is kept to read; talking on starts a new one. */
function ArchivedConversation({ project }: { project: Project }) {
  const { ui } = useApp();
  const supported = useHost((s) => !!s.capabilities?.features.includes(CONVERSATIONS_FEATURE));
  return (
    <div className="composer">
      <Notice tone="info" title="Archived" className="composer__notice"
        action={<Button size="sm" variant="secondary" icon={MessageSquarePlus} disabled={!supported} onClick={() => ui.newConversation(project.project_id)}>New conversation</Button>}>
        This conversation is kept to read. To keep chatting, start a new one.
      </Notice>
    </div>
  );
}
