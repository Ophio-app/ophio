// SPDX-License-Identifier: Apache-2.0
//
// Talk's root while no conversation is open, as a messaging app's chats
// (D-065): New conversation and the project's checklist, then its conversations, the latest
// activity first, each saying which model it uses, whether a reply waits
// unread, and how it stands when that's worth a word. Archived ones are a tab of their own, kept
// to read. Tasks live in Work, one row away at the end. The tabs are up and
// the project's name heads it, opening the other projects.

import { Archive, ListTodo, MessageSquarePlus } from "lucide-react";
import { useState } from "react";
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

export function TalkConversations({ project }: { project: Project }) {
  const { ui } = useApp();
  const pid = project.project_id;
  const all = useHost((s) => projectTasks(s, pid).filter((t) => t.conversation), shallowEqual);
  const open = all.filter((t) => t.state !== "archived");
  const archived = all.filter((t) => t.state === "archived");
  const [part, setPart] = useState<"open" | "archived">("open");
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
  const start = () => ui.newConversation(pid);

  const row = (t: Task) => {
    const run = latestRun(t, runs);
    const reason = reasons[t.task_id] ?? null;
    const st = conversationBadge(t, run, undefined, reason);
    const draft = !!drafts[`${pid}/${t.task_id}`]?.text.trim();
    const model = agentLabel(harnesses, { harness: t.harness, model: t.model, effort: t.effort });
    return (
      <div key={t.task_id} className="talk-task-row">
        <ListRow
          title={t.title}
          subtitle={reason ?? (isActiveTask(t) ? taskStatus(t, run, controls[t.target_id ?? ""]) : null) ?? model}
          meta={<Age at={t.updated_at} suffix=" ago" />}
          trailing={
            <span className="row__pills">
              {unread.includes(t.task_id) && <Pill tone="accent">New reply</Pill>}
              {draft && <Pill>Draft</Pill>}
              {st && <Pill className={online ? undefined : "row__reported"} tone={online ? st.tone : "neutral"}>{!online && "Last reported · "}{st.label}</Pill>}
            </span>
          }
          onClick={() => ui.selectTask(pid, t.task_id)}
        />
        {!isActiveTask(t) && t.state !== "archived" && (
          <IconButton icon={Archive} label={`Archive ${t.title}`} disabled={!archive.can}
            onClick={() => archive.ask([t], () => toast(`Archived “${t.title}”`))} />
        )}
      </div>
    );
  };

  return (
    <Screen header={<ContextHeader heading />} className="talk-tasks">
      {!supported && <Notice tone="unknown">{CONVERSATIONS_UPDATE}</Notice>}
      <Section>
        <div className="group">
          <ListRow icon={MessageSquarePlus} title="New conversation" disabled={!supported} onClick={start} />
          <ChecklistRow project={project} />
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
      <Section>
        <div className="group">
          <WorkDoor project={project} />
        </div>
      </Section>
    </Screen>
  );
}

/** Tasks are Work's: a row saying what runs and waits there, which goes to it. */
function WorkDoor({ project }: { project: Project }) {
  const { ui } = useApp();
  const pid = project.project_id;
  const running = useHost((s) => projectTasks(s, pid).filter((t) => !t.conversation && isTaskRunning(s, t)).length);
  const needs = useHost((s) => listedAttention(s, pid).filter((a) => !s.tasks[a.task_id ?? ""]?.conversation).length);
  const counts = [running && `${running} running`, needs && `${needs} ${needs === 1 ? "needs" : "need"} you`].filter(Boolean).join(" · ");
  return (
    <ListRow icon={ListTodo} title="Go to Tasks" subtitle={counts || "Start tasks, see results and fix issues"} chevron onClick={() => ui.selectTab("work")} />
  );
}
