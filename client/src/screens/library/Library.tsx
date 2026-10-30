// SPDX-License-Identifier: Apache-2.0
//
// Library (D-057): the agents and skills a task can start from, and the files
// they and the person use, as Agents, Skills and Files segments, one on screen
// at a time, opening on Agents. The tab names the screen, so it carries no
// title. The scratch pad is one row; an agent or skill starts its task in
// Review with a tap, its file and defaults behind About; and a list long
// enough to search can be. At Home it lists everything; a project chosen in
// the header narrows each list to that project's own, then Øphio's, which
// every project has as its defaults.

import { useState } from "react";
import { Bot, Info, NotebookPen, Plus, Sparkles } from "lucide-react";
import { SOURCE_LABEL } from "../../app/launchers";
import { IconButton } from "../../design/Button";
import { Segmented } from "../../design/Chip";
import { TextField } from "../../design/Field";
import { ListRow } from "../../design/ListRow";
import type { CatalogEntry, CatalogSource, HarnessInfo } from "../../protocol/types";
import { useStore } from "../../state/store";
import { Screen } from "../../shell/Screen";
import { entryAsks, matching, useAgents, type Agents } from "../launch/agents";
import type { RouteProps } from "../types";
import { LibraryFiles } from "./Files";
import { ConnectionNote, useLibrary, useLibraryScope, useScreenBody } from "./shared";
import { Checking, DisabledSection } from "./Disabled";
import { listedDisabled, useDisabled } from "../../app/disabled";
import "./library.css";

/** Past this many, a list gets a search. */
const SEARCHABLE = 8;

type Part = "agents" | "skills" | "files";

/** Found in a project's own folders rather than the person's or Øphio's. */
const PROJECT_SOURCES: CatalogSource[] = ["claude_project", "codex_project"];
/** Øphio's own, built in or made in it: every project lists them after its own. */
const OPHIO_SOURCES: CatalogSource[] = ["ophio_setup", "ophio"];

export default function Library(_props: RouteProps<"library">) {
  const bodyRef = useScreenBody();
  const at = useStore(useLibrary(), (s) => s.roots?.at);
  const a = useAgents();
  const scope = useLibraryScope();
  const [part, setPart] = useState<Part>("agents");
  const [query, setQuery] = useState("");
  const own = (entries: CatalogEntry[]) => scope
    ? [...entries.filter((e) => PROJECT_SOURCES.includes(e.source)), ...entries.filter((e) => OPHIO_SOURCES.includes(e.source))]
    : entries;
  // What is disabled in Øphio is held apart, in its own section below.
  const d = useDisabled();
  const live = (entries: CatalogEntry[]) => entries.filter((e) => (!d.supported || !d.isDisabled(e.disable?.state === "can" ? e.disable.key ?? e.host_path : e.host_path, listedDisabled(e.disable), e.disable?.state === "can" && !!e.disable.key)));
  const agents = live(own(a.agents)), skills = live(own(a.skills));
  const listed = a.blocked ? null : part === "agents" ? agents : skills;
  const shown = listed && matching(listed, query);
  const searching = !!query.trim();
  return (
    <Screen className="library-screen launch-screen" bodyRef={bodyRef}>
      <h1 className="visually-hidden">Library</h1>
      <ConnectionNote at={at} />
      <div className="library-body">
        <Segmented<Part>
          label="Show"
          className="agents-parts"
          value={part}
          onChange={(p) => { setPart(p); setQuery(""); }}
          // Each count is the rows its list shows; Scratch pad is one of Home's agents.
          options={[
            { value: "agents", label: agents.length ? `Agents ${agents.length + (scope ? 0 : 1)}` : "Agents" },
            { value: "skills", label: skills.length ? `Skills ${skills.length}` : "Skills" },
            { value: "files", label: "Files" },
          ]}
        />
        {part === "files" ? <LibraryFiles scope={scope} /> : (
          <div className="agents-body">
            {a.failed}
            {a.blocked}
            {listed && shown && (
              <>
                {a.refreshing && <Checking refreshing />}
                {listed.length > SEARCHABLE && <Search label={part === "agents" ? "Search agents" : "Search skills"} value={query} onChange={setQuery} />}
                {(shown.length > 0 || part === "agents" && !searching && !scope) && (
                  <div className="group">
                    {part === "agents" && !searching && !scope && <ScratchRow a={a} />}
                    {shown.map((e) => <EntryRow key={key(e)} entry={e} a={a} />)}
                    {/* A new agent is the person's own, which Home lists. */}
                    {part === "agents" && !searching && !scope && <NewAgentRow a={a} />}
                  </div>
                )}
                {!listed.length && <p className="fine">{scope
                  ? `${scope.name} has no ${part} of its own, and Øphio has none to add. Home lists all of yours.`
                  : part === "agents" ? a.none.agent : a.none.skill}</p>}
                {searching && !shown.length && <p className="fine">No {part === "agents" ? "agent" : "skill"} matches “{query.trim()}”.</p>}
                {a.problems}
                <DisabledSection projectId={scope?.project_id} />
              </>
            )}
          </div>
        )}
      </div>
      {a.sheets}
    </Screen>
  );
}

/** "Claude Code · Opus (latest) · High effort": where an entry is from and what
 * it asks for, naming its agent only when the folder doesn't already. */
function entryFrom(entry: CatalogEntry, harnesses: HarnessInfo[]): string {
  const source = SOURCE_LABEL[entry.source];
  const { agent, model, effort } = entryAsks(entry, harnesses);
  return [source, agent && !source.includes(agent) ? agent : null, model, effort].filter(Boolean).join(" · ");
}

function EntryRow({ entry, a }: { entry: CatalogEntry; a: Agents }) {
  return (
    <div className="agents-row">
      <ListRow
        icon={entry.kind === "agent" ? Bot : Sparkles}
        title={entry.name}
        subtitle={
          <>
            {entry.description && <span className="launch-row__desc">{entry.description}</span>}
            <span className="launch-row__from">{entryFrom(entry, a.harnesses)}</span>
          </>
        }
        ariaLabel={`Start from ${entry.name}`}
        onClick={() => a.start(entry)}
      />
      <IconButton icon={Info} label={`About ${entry.name}`} onClick={() => a.about(entry)} />
    </div>
  );
}

function ScratchRow({ a }: { a: Agents }) {
  return <ListRow icon={NotebookPen} title="Scratch pad" subtitle="A clean slate, with no instructions added" onClick={a.scratch} />;
}

function NewAgentRow({ a }: { a: Agents }) {
  if (!a.canMake) return null;
  return <ListRow icon={Plus} title="New agent" subtitle="Name it, then write its instructions" onClick={a.make} disabled={!a.online} />;
}

function Search({ label, value, onChange }: { label: string; value: string; onChange: (v: string) => void }) {
  return <TextField label={label} hideLabel placeholder={label} value={value} onChange={onChange} type="search" />;
}

const key = (e: CatalogEntry) => `${e.kind}/${e.source}/${e.id}`;
