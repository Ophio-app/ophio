// SPDX-License-Identifier: Apache-2.0
//
// Work's root (D-056, D-065): New first, then Open, Finished and Archived as
// tabs, one on screen at a time, so what is still going is never mixed with
// what has ended. Open leads with what needs the person, the first of it
// outlined as the one to answer first, then what runs and Next up. Finished is
// where tasks are archived, one at a time or all at once; Archived keeps them
// to read. Work holds the tasks; Talk holds conversations. The project's name
// is the header's, so the body never repeats it, and the project's own doors
// are gathered at the end.

import { ReportNeedsYou } from "../../reports/Questions";
import { Archive, Bot, Inbox as InboxIcon, Settings, Workflow } from "lucide-react";
import { useState, type ReactNode } from "react";
import { shallowEqual, useApp, useHost } from "../../app/context";
import { Age } from "../../design/Age";
import { Button, IconButton } from "../../design/Button";
import { Segmented } from "../../design/Chip";
import { TextField } from "../../design/Field";
import { ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import type { Project, Task } from "../../protocol/types";
import { ChecklistRow, useShortScreen } from "../checklist/RequestChecklist";
import { WaitingHandoffs } from "../contribute/Waiting";
import { GithubDoors } from "../github/GithubDoors";
import { NewWork } from "./NewWork";
import { useArchive } from "./archive";
import { attentionGroups } from "./attention";
import { taskMatches } from "./logic";
import { AttentionList, EarlierTasks, listedAttention, TaskRow } from "./shared";
import { StageRow } from "./StageRow";

export interface WorkView {
  project: Project;
  /** The project has any tasks at all, here or still on the computer. */
  any: boolean;
  /** The computer holds earlier tasks this device hasn't read yet. */
  older: boolean;
  /** Running now, queued ones and ones listed under Needs you aside. */
  running: Task[];
  /** Running ones listed under Needs you instead. */
  runningNeedsYou: number;
  /** Tasks that haven't ended: running, waiting in Next up or on the person. */
  open: number;
  /** Tasks whose run ended without a known result, to check. */
  unsettled: Task[];
  /** Ended tasks not yet archived, newest first. */
  finished: Task[];
  /** Archived tasks, newest first. */
  archived: Task[];
  /** Something waits in Next up. */
  queued: boolean;
  /** Next up as a row: what waits, or that nothing does. */
  nextUp: ReactNode;
  /** What Next up's door says while nothing waits. */
  nextUpNote: string;
  /** Why no work task can start, while that needs the person. */
  held: ReactNode;
  brief: { lead: string; more: string[]; since: number; gap: boolean };
  inboxCount: number;
  /** The pictures, videos and pages agents showed in the project. */
  shownCount: number;
  others: { project: Project; count: number; reason: string }[];
  onSettings: () => void;
  onSelect: (projectId: string) => void;
  /** Shown in place of everything else for a project with no tasks yet. */
  empty: ReactNode;
}

type Part = "open" | "finished" | "archived";

/** What Running says while it lists nothing. */
function runningElsewhere(needsYou: number): string {
  if (!needsYou) return "No task is running in this project.";
  return needsYou === 1 ? "The task running here needs you, so it’s under Needs you." : `${needsYou} tasks running here need you, so they’re under Needs you.`;
}

export function WorkRoot({ v }: { v: WorkView }) {
  const { needs } = useCounts(v);
  // Opens on Open while anything is, here or waiting in another project.
  const [part, setPart] = useState<Part>(() => (needs || v.others.length || v.open || !v.finished.length ? "open" : "finished"));
  // A short screen has room for little above the fold: what agents ask right
  // now comes first there, and the checklist follows it.
  const short = useShortScreen();
  const checklist = short && <div className="group"><ChecklistRow project={v.project} /></div>;
  // A project with nothing yet still points to what waits in another.
  if (!v.any) return <><NewWork project={v.project} checklist={!short} /><WaitingHandoffs /><ReportNeedsYou /><OtherProjects v={v} />{checklist}{v.empty}<ProjectDoors v={v} bare /></>;
  const id = v.project.project_id;
  return (
    <>
      <NewWork project={v.project} checklist={!short} />
      <Segmented<Part>
        label="Show"
        className="work-parts"
        value={part}
        onChange={setPart}
        options={[
          { value: "open", label: v.open ? `Open ${v.open}` : "Open" },
          { value: "finished", label: v.finished.length ? `Finished ${v.finished.length}` : "Finished" },
          { value: "archived", label: "Archived" },
        ]}
      />
      {part === "open" ? (
        <div className="work-part">
          <WaitingHandoffs />
          <ReportNeedsYou />
          {needs > 0 && <Section title="Needs you"><AttentionList projectId={id} lead /></Section>}
          <OtherProjects v={v} />
          {checklist}
          <Section title="Running">
            {v.held}
            {!v.running.length && !v.held && <p className="work-muted">{runningElsewhere(v.runningNeedsYou)}</p>}
            <div className="group">
              {v.running.map((t) => <TaskRow key={t.task_id} task={t} />)}
              <LiveAgents />
            </div>
          </Section>
          {v.unsettled.length > 0 && (
            <Section title="To check">
              <div className="group">{v.unsettled.map((t) => <TaskRow key={t.task_id} task={t} />)}</div>
            </Section>
          )}
          <Section title="Next up">{v.nextUp}</Section>
        </div>
      ) : part === "finished" ? (
        <>{checklist}<Finished v={v} /></>
      ) : (
        <>{checklist}<Archived v={v} /></>
      )}
      <ProjectDoors v={v} />
    </>
  );
}

/** Ended tasks, newest first: Unread, those not opened since they ended,
 * then Read. Each task has Archive, and each part its own Archive all. */
function Finished({ v }: { v: WorkView }) {
  const id = v.project.project_id;
  const unread = useHost((s) => [...new Set(listedAttention(s, id, true).flatMap((a) => a.task_id ?? []))].sort(), shallowEqual);
  const archive = useArchive();
  const parts = [
    { title: "Unread", tasks: v.finished.filter((t) => unread.includes(t.task_id)) },
    { title: "Read", tasks: v.finished.filter((t) => !unread.includes(t.task_id)) },
  ];
  return (
    <div className="work-part">
      <BriefRow brief={v.brief} />
      {parts.map(({ title, tasks }) => tasks.length > 0 && (
        <Section key={title} title={title} action={tasks.length > 1 && (
          <Button size="sm" variant="secondary" icon={Archive} disabled={!archive.can}
            aria-label={`Archive all ${tasks.length} ${title.toLowerCase()}`}
            onClick={() => archive.ask(tasks)}>Archive all {tasks.length}</Button>
        )}>
          <ul className="group" role="list">
            {tasks.map((t) => (
              <li key={t.task_id} className="work-task-row">
                <TaskRow task={t} />
                <IconButton icon={Archive} label={`Archive ${t.title}`} disabled={!archive.can}
                  onClick={() => archive.ask([t])} />
              </li>
            ))}
          </ul>
        </Section>
      ))}
      {!v.finished.length && !v.older && <p className="work-muted">Tasks move here when they finish, fail or are stopped.</p>}
      <EarlierTasks projectId={id} />
      {archive.sheet}
    </div>
  );
}

/** Archived tasks, kept to read, and a search once there are a few. */
function Archived({ v }: { v: WorkView }) {
  const [search, setSearch] = useState("");
  const shown = v.archived.filter((t) => taskMatches(t, search));
  return (
    <div className="work-part">
      {v.archived.length > 5 && <TextField label="Find an archived task" placeholder="Title or request" value={search} onChange={setSearch} type="search" />}
      {shown.length > 0
        ? <div className="group">{shown.map((t) => <TaskRow key={t.task_id} task={t} />)}</div>
        : !v.older && <p className="work-muted">{search.trim() ? "No archived task matches." : "Tasks you archive from Finished stay here, with their history, to read."}</p>}
      <EarlierTasks projectId={v.project.project_id} />
    </div>
  );
}

/** Everything the project alerted about and nobody has looked at yet: what
 * waits for the person first, then, unless `results` is false, results to look at. */
export function NeedsYouList({ projectId, results: withResults = true }: { projectId: string; results?: boolean }) {
  const results = useHost((s) => withResults && listedAttention(s, projectId, true).length > 0);
  return <>
    <ReportNeedsYou />
    <AttentionList projectId={projectId} lead />
    {results && <Section title="To look at"><AttentionList projectId={projectId} review quiet /></Section>}
  </>;
}

/** How many things need the person, as the list below groups them. */
function useCounts(v: WorkView) {
  const id = v.project.project_id;
  const open = useHost((s) => listedAttention(s, id), shallowEqual);
  return { needs: attentionGroups(open).length };
}

/** What happened since the person last looked, in a line that opens. */
function BriefRow({ brief }: { brief: WorkView["brief"] }) {
  if (!brief.more.length && !brief.since && !brief.gap) return <p className="work-brief-row work-brief-row--flat">{brief.lead}</p>;
  return (
    <details className="work-brief-row">
      <summary>{brief.lead}</summary>
      {brief.more.length > 0 && <ul>{brief.more.map((s) => <li key={s}>{s}</li>)}</ul>}
      {brief.since > 0 && <p className="work-caption">Last looked <Age at={brief.since} suffix=" ago" /></p>}
      {brief.gap && (
        <Notice tone="unknown">
          Some events are no longer available. This brief covers the records the computer supplied.
        </Notice>
      )}
    </details>
  );
}

/** Other projects waiting on the person, one row each that goes there; the
 * name stands out, as a project that isn't the one shown. */
function OtherProjects({ v }: { v: WorkView }) {
  if (!v.others.length) return null;
  return (
    <Section title="Other projects">
      <div className="group">
        {v.others.map((g) => (
          <ListRow key={g.project.project_id} title={<>{g.count} in <strong className="work-other">{g.project.name}</strong></>} subtitle={g.reason} chevron
            onClick={() => v.onSelect(g.project.project_id)} />
        ))}
      </div>
    </Section>
  );
}

function inboxNote(v: WorkView) {
  const here = `${v.inboxCount} on this device`;
  return v.shownCount ? `${v.shownCount} from agents · ${here}` : `${here} · none from agents yet`;
}

/** Every agent at work on the computer, in any project, after this one's. */
function LiveAgents() {
  const { ui } = useApp();
  return <ListRow icon={Bot} title="Live agents" subtitle="Every agent working on this computer" chevron onClick={() => ui.push({ screen: "agents" })} />;
}

/** Everything about the project that isn't today's work, behind one door
 * each. `bare` is a project with nothing yet, whose body has no Running or
 * Next up, so their doors join these. */
function ProjectDoors({ v, bare = false }: { v: WorkView; bare?: boolean }) {
  const { ui } = useApp();
  const p = v.project;
  const where = [p.vcs?.branch, p.vcs?.dirty_files != null ? `${p.vcs.dirty_files} changed ${p.vcs.dirty_files === 1 ? "file" : "files"}` : null]
    .filter(Boolean).join(" · ");
  return (
    <Section title="Project">
      <div className="group">
        <GithubDoors project={p} />
        {bare && <ListRow icon={Workflow} title="Next up" subtitle={v.nextUpNote} chevron onClick={() => ui.push({ screen: "next_up" })} />}
        {bare && <LiveAgents />}
        <ListRow icon={InboxIcon} title="Capture inbox" subtitle={inboxNote(v)} chevron onClick={() => ui.push({ screen: "inbox" })} />
        <StageRow project={p} />
        <ListRow icon={Settings} title="Project settings" subtitle={where || undefined} chevron onClick={v.onSettings} />
      </div>
    </Section>
  );
}
