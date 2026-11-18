// SPDX-License-Identifier: Apache-2.0
//
// Talk's root while no conversation is open, as a messaging app's chats
// (D-065): New conversation and the project's checklist, then its conversations, the latest
// activity first, each saying which model it uses, whether a reply waits
// unread, and how it stands when that's worth a word. Archived ones are a tab of their own, kept
// to read. Tasks live in Work, one row away at the end. The tabs are up and
// the project's name heads it, opening the other projects.

import { Archive, ListTodo, MessageSquarePlus } from "lucide-react";
import { useLayoutEffect, useRef, useState } from "react";
import { useTalkAll } from "../../app/talkScope";
import { ProjectSheet } from "../../shell/ProjectSheet";
import { shallowEqual, useApp, useHost, useLink, useUi } from "../../app/context";
import { Age } from "../../design/Age";
import { IconButton } from "../../design/Button";
import { Pill, Segmented } from "../../design/Chip";
import { ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { toast } from "../../design/Toast";
import type { Project, Task } from "../../protocol/types";
import { isActiveTask, isTaskRunning, projectTasks, replyAttention } from "../../state/projection";
import { ContextHeader } from "../../shell/ContextHeader";
import { ChecklistRow } from "../checklist/RequestChecklist";
import { Screen } from "../../shell/Screen";
import { latestRun } from "../work/logic";
import { listedAttention } from "../work/shared";
import { taskAttentionReasons, taskStatus } from "../work/taskActivity";
import { useArchive } from "../work/archive";
import { CONVERSATIONS_FEATURE, CONVERSATIONS_UPDATE, conversationBadge } from "./conversation";
import { agentLabel } from "./intent";

export interface ConversationListPosition { part: "open" | "archived"; top: number }

export function TalkConversations({ project, positions }: { project: Project; positions?: Map<string, ConversationListPosition> }) {
  const { ui } = useApp();
  const across = useTalkAll();
  const pid = across ? null : project.project_id;
  const projects = useHost((s) => s.projects);
  const chosen = useUi((s) => s.projectId);
  const destination = (chosen ? projects[chosen] : undefined) ?? Object.values(projects).find((p) => p.home);
  const scope = pid ?? "all";
  const body = useRef<HTMLDivElement>(null);
  const [picking, setPicking] = useState(false);
  const all = useHost((s) => Object.values(s.tasks)
    .filter((t) => t.conversation && (pid ? t.project_id === pid : !!s.projects[t.project_id]))
    .sort((a, b) => b.updated_at - a.updated_at), shallowEqual);
  const open = all.filter((t) => t.state !== "archived");
  const archived = all.filter((t) => t.state === "archived");
  const [part, setPart] = useState<"open" | "archived">(() => positions?.get(scope)?.part ?? "open");
  useLayoutEffect(() => {
    const el = body.current;
    if (!el) return;
    el.scrollTop = positions?.get(scope)?.top ?? 0;
    return () => { positions?.set(scope, { part, top: el.scrollTop }); };
  }, [scope, positions, part]);
  const conversations = part === "archived" && archived.length ? archived : open;
  const runs = useHost((s) => s.runs);
  const controls = useHost((s) => s.control);
  const harnesses = useHost((s) => s.harnesses);
  const reasons = useHost((s) => taskAttentionReasons(s, conversations), shallowEqual);
  const unread = useHost((s) => replyAttention(s).map((a) => a.task_id ?? ""), shallowEqual);
  const drafts = useUi((s) => s.drafts);
  const online = useLink().state === "online";
  const supported = useHost((s) => !!s.capabilities?.features.includes(CONVERSATIONS_FEATURE));
  const archive = useArchive();
  const start = () => {
    const target = across ? destination : project;
    if (target) ui.newConversation(target.project_id);
    else setPicking(true);
  };

  const row = (t: Task) => {
    const run = latestRun(t, runs);
    const reason = reasons[t.task_id] ?? null;
    const st = conversationBadge(t, run, undefined, reason);
    const draft = !!drafts[`${t.project_id}/${t.task_id}`]?.text.trim();
    const model = agentLabel(harnesses, { harness: t.harness, model: t.model, effort: t.effort });
    const agent = harnesses.find((h) => h.kind === t.harness)?.display_name ?? agentLabel(harnesses, { harness: t.harness, model: null, effort: null });
    const detail = reason ?? (isActiveTask(t) ? taskStatus(t, run, controls[t.target_id ?? ""]) : null) ?? model;
    return (
      <div key={t.task_id} className="talk-task-row">
        <ListRow
          title={t.title}
          subtitle={across ? <>
            <span className="talk-conversation-line" aria-hidden><bdi className="talk-conversation-project">{projects[t.project_id]?.name}</bdi><span>·</span><span className="talk-conversation-agent">{agent}</span></span>
            <span className="visually-hidden">{projects[t.project_id]?.name}. {model}{detail !== model ? `. ${detail}` : ""}</span>
          </> : detail}
          meta={<Age at={t.updated_at} suffix=" ago" />}
          trailing={
            <span className="row__pills">
              {unread.includes(t.task_id) && <Pill tone="accent">New reply</Pill>}
              {draft && <Pill>Draft</Pill>}
              {st && <Pill className={online ? undefined : "row__reported"} tone={online ? st.tone : "neutral"}>{!online && "Last reported · "}{st.label}</Pill>}
            </span>
          }
          onClick={() => ui.selectTask(t.project_id, t.task_id)}
        />
        {!isActiveTask(t) && t.state !== "archived" && (
          <IconButton icon={Archive} label={`Archive ${t.title}`} disabled={!archive.can}
            onClick={() => archive.ask([t], () => toast(`Archived “${t.title}”`))} />
        )}
      </div>
    );
  };

  return (
    <Screen header={<ContextHeader heading conversations />} className="talk-tasks" bodyRef={body}>
      {!supported && <Notice tone="unknown">{CONVERSATIONS_UPDATE}</Notice>}
      <Section>
        <div className="group">
          <ListRow icon={MessageSquarePlus} title="New conversation" subtitle={across && destination ? <span className="talk-conversation-destination">In {destination.name}</span> : undefined} disabled={!supported} onClick={start} />
          {!across && <ChecklistRow project={project} />}
        </div>
      </Section>
      {archived.length > 0 && (
        <Segmented<"open" | "archived">
          label="Show"
          value={part}
          onChange={setPart}
          options={[
            { value: "open", label: open.length ? `Open ${open.length}` : "Open" },
            { value: "archived", label: `Archived ${archived.length}` },
          ]}
        />
      )}
      <Section title="Conversations">
        {conversations.length > 0
          ? <div className="group">{conversations.map(row)}</div>
          : <p className="talk-muted">{archived.length ? "No open conversations." : "No conversations yet."}</p>}
      </Section>
      {archive.sheet}
      <ProjectSheet open={picking} conversations onClose={() => setPicking(false)} onChooseProject={(p) => ui.newConversation(p.project_id)} />
      <Section>
        <div className="group">
          <WorkDoor projectId={pid} />
        </div>
      </Section>
    </Screen>
  );
}

/** Tasks are Work's: a row saying what runs and waits there, which goes to it. */
function WorkDoor({ projectId: pid }: { projectId: string | null }) {
  const { ui } = useApp();
  const running = useHost((s) => (pid ? projectTasks(s, pid) : Object.values(s.tasks)).filter((t) => !t.conversation && isTaskRunning(s, t)).length);
  const needs = useHost((s) => listedAttention(s, pid).filter((a) => !s.tasks[a.task_id ?? ""]?.conversation).length);
  const counts = [running && `${running} running`, needs && `${needs} ${needs === 1 ? "needs" : "need"} you`].filter(Boolean).join(" · ");
  return (
    <ListRow icon={ListTodo} title="Go to Tasks" subtitle={counts || "Start tasks, see results and fix issues"} chevron onClick={() => ui.selectTab("work")} />
  );
}
