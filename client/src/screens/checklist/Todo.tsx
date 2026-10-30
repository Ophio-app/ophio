// SPDX-License-Identifier: Apache-2.0
//
// The person's checklist and the agents waiting for them, in one destination.
// The header's project picker chooses the project, or All projects on this
// tab; choosing a project elsewhere returns to that project.

import { useEffect, useState } from "react";
import { ListTodo } from "lucide-react";
import { allItems, CHECKLIST_UPDATE, stillShown, useChecklists } from "../../app/checklist";
import { shallowEqual, useApp, useHost, useLink, useUi } from "../../app/context";
import { Button } from "../../design/Button";
import { Empty, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import type { ChecklistItem, Project } from "../../protocol/types";
import { ContextHeader } from "../../shell/ContextHeader";
import { NotAllowed, useMay } from "../../shell/NotAllowed";
import { Screen } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { useQueue } from "../work/useQueue";
import { AgentWorkEntry, deriveAgentWork, type AgentWorkEntryData } from "./AgentWork";
import { ChecklistBody, ChecklistEntry } from "./Checklist";
import { RequestChecklist } from "./RequestChecklist";
import { checklistOrder, compareTodo, type TodoOrder } from "./todoOrder";
import "./todo.css";

/** Queue queries populate the host cache; events keep it current afterward. */
function QueueReader({ project, shown }: { project: Project; shown: boolean }) {
  const { error, reload } = useQueue(project.project_id);
  const online = useLink().state === "online";
  return shown && error ? <Notice tone="unknown" title={`Next up · ${project.name}`}
    action={<Button size="sm" disabled={!online} onClick={reload}>Try again</Button>}>{error}</Notice> : null;
}

export default function Todo(_: RouteProps<"todo">) {
  const allowed = useMay("tasks");
  return <Screen header={<ContextHeader heading />} className="checklist-screen todo-screen">
    {allowed ? <TodoContents /> : <NotAllowed what="see and tick off checklists" permission="tasks" />}
  </Screen>;
}

type AllEntry = TodoOrder & ({ kind: "item"; item: ChecklistItem } | { kind: "task"; entry: AgentWorkEntryData });

function TodoContents() {
  const { ui, profile } = useApp();
  const online = useLink().state === "online";
  const projectId = useUi((s) => s.projectId);
  const all = useUi((s) => s.todoAll);
  const itemId = useUi((s) => s.todoItemId);
  const projects = useHost((s) => Object.values(s.projects), shallowEqual);
  // Text/output streaming does not change any input used by these entries.
  const host = useHost((s) => s, (a, b) => a.projects === b.projects && a.tasks === b.tasks
    && a.queues === b.queues && a.attention === b.attention && a.approvals === b.approvals
    && a.runs === b.runs && a.control === b.control && a.targets === b.targets
    && a.capabilities === b.capabilities);
  const ids = projects.map((p) => p.project_id);
  const lists = useChecklists(ids);
  const [allFocus, setAllFocus] = useState<{ projectId: string; itemId: string } | null>(null);
  const [showDone, setShowDone] = useState(false);
  // Gone to by a link, an item shows however long ago it was done.
  const [kept, setKept] = useState<ReadonlySet<string>>(() => new Set());
  const supported = !!host.capabilities?.features.includes("checklist_v1");
  const work = new Map(projects.map((p) => [p.project_id, deriveAgentWork(host, p.project_id, lists.get(p.project_id)?.list)]));
  const choose = (id: string) => { ui.selectProject(id); ui.setTodoAll(false); ui.setTodoItem(null); };
  const go = (id: string, item?: string) => {
    if (all && item) {
      if (lists.get(id)?.list?.sections.some((s) => s.items.some((i) => i.item_id === item && i.done_at !== undefined))) setShowDone(true);
      setKept((k) => new Set(k).add(`${id}/${item}`)); setAllFocus({ projectId: id, itemId: item });
    }
    else ui.openChecklist(id, item);
  };
  const projectLabel = (project: Project) => <button className="todo-project-link" type="button" onClick={() => choose(project.project_id)}>{project.name}</button>;
  const entries: AllEntry[] = projects.flatMap((p) => {
    const list = lists.get(p.project_id)?.list;
    const items: AllEntry[] = list ? checklistOrder(list).filter((value) => stillShown(value.item) || kept.has(`${p.project_id}/${value.id}`))
      .map((value) => ({ ...value, kind: "item" as const })) : [];
    return [...items, ...(work.get(p.project_id) ?? []).map((entry) => ({ ...entry, kind: "task" as const, entry }))];
  }).sort(compareTodo);
  const renderEntry = (entry: AllEntry) => {
    const project = host.projects[entry.projectId]!;
    const key = `${entry.projectId}/${entry.kind}/${entry.id}`;
    if (entry.kind === "task") return <AgentWorkEntry key={key} entry={entry.entry} projectLabel={projectLabel(project)} onChecklist={go} />;
    const value = lists.get(entry.projectId)!;
    return <ChecklistEntry key={key} list={value.list!} item={entry.item} project={project} send={value.send}
      focus={allFocus?.projectId === entry.projectId && allFocus.itemId === entry.id}
      onGo={(id) => go(entry.projectId, id)} projectLabel={projectLabel(project)} />;
  };
  const project = projectId ? host.projects[projectId] : undefined;
  const value = projectId ? lists.get(projectId) : undefined;
  // Consume a focus request after its list mounts. Clearing it lets the same
  // link reopen a fold later, without reopening it after an ordinary tick.
  useEffect(() => { if (itemId && value?.list && allItems(value.list).some((item) => item.item_id === itemId)) ui.setTodoItem(null); }, [itemId, value?.list, ui]);
  useEffect(() => { if (allFocus) setAllFocus(null); }, [allFocus]);
  const agentEntries = projectId ? work.get(projectId) ?? [] : [];
  const workSection = (entries: AgentWorkEntryData[], later = false) => entries.length > 0 && <Section title={later ? "Next up" : "Agents’ work"}>
    <ol className="todo-agent-list">{entries.map((entry) => <AgentWorkEntry key={entry.id} entry={entry} onChecklist={go} />)}</ol>
  </Section>;
  const errors = all ? projects.filter((p) => lists.get(p.project_id)?.error) : project && value?.error ? [project] : [];
  const unread = supported && [...lists.values()].some((v) => v.list === undefined && !v.error);
  return <>
    {projects.map((p) => <QueueReader key={p.project_id} project={p} shown={all || p.project_id === projectId} />)}
    {!supported && <Notice tone="warn" title="Update the computer">{CHECKLIST_UPDATE}</Notice>}
    {errors.map((p) => <Notice key={p.project_id} tone="unknown" title={p.name} action={<Button size="sm" disabled={!lists.get(p.project_id)?.online} onClick={lists.get(p.project_id)?.reload}>Try again</Button>}>{lists.get(p.project_id)?.error}</Notice>)}
    {all ? <>
      <h2 className="todo-heading">All to-dos</h2>
      {entries.some((entry) => !entry.done) ? <ol className="cl-items todo-all">{entries.filter((entry) => !entry.done).map(renderEntry)}</ol>
        : !unread && errors.length === 0 && <Empty icon={ListTodo} title="Nothing to do here">Your checklists and agents’ work appear here.</Empty>}
      {unread && (online ? <p className="settings-meta" aria-busy="true">Reading the checklists…</p>
        : <Notice tone="unknown">Connect to {profile.name} to read the checklists.</Notice>)}
      {entries.some((entry) => entry.done) && <section className="todo-done">
        <Button variant="quiet" aria-expanded={showDone} onClick={() => setShowDone(!showDone)}>Done ({entries.filter((entry) => entry.done).length})</Button>
        {showDone && <ol className="cl-items todo-all">{entries.filter((entry) => entry.done).map(renderEntry)}</ol>}
      </section>}
    </> : project && value ? <>
      {value.list && supported ? <ChecklistBody key={project.project_id} list={value.list} project={project} send={value.send} focus={itemId ?? undefined}
        beforeSections={workSection(agentEntries.filter((entry) => entry.tier === 0))}
        afterSections={workSection(agentEntries.filter((entry) => entry.tier !== 0), true)} /> : <>
        {workSection(agentEntries.filter((entry) => entry.tier === 0))}
        {supported && value.list === undefined && !value.error && (value.online
          ? <div className="screen__loading" aria-busy="true" aria-label="Reading the checklist" />
          : <Notice tone="unknown">Connect to {profile.name} to read the checklist.</Notice>)}
        {supported && value.list === null && <Empty icon={ListTodo} title="No checklist yet" action={<RequestChecklist project={project} />}>
          When an agent needs you to do something yourself, such as sign in, make a key or choose, it goes here with every step. Ask an agent for one now.
        </Empty>}
        {workSection(agentEntries.filter((entry) => entry.tier !== 0), true)}
      </>}
    </> : <Empty icon={ListTodo} title="Choose a project">Tap the project name at the top to choose one, or All projects to see every project’s to-dos.</Empty>}
  </>;
}
