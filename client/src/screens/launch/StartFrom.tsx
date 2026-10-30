// SPDX-License-Identifier: Apache-2.0
//
// Choosing what a new task in Talk starts from: the same agents and skills as
// Library, read from the computer when the sheet opens. A new conversation
// opens it on agents or skills alone.

import { useState } from "react";
import { Bot, NotebookPen, Sparkles } from "lucide-react";
import { useHost, useLink } from "../../app/context";
import { ONE_OFF_DRAFT, SCRATCH, SOURCE_LABEL, entryAgent, launchStart, useCatalog, type LaunchStart } from "../../app/launchers";
import { listedDisabled, useDisabled } from "../../app/disabled";
import type { AgentChoice } from "../../app/ui";
import { Button } from "../../design/Button";
import { Segmented } from "../../design/Chip";
import { Empty, ListRow } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { TextField } from "../../design/Field";
import type { CatalogKind } from "../../protocol/types";
import { matching } from "./agents";
import "./launch.css";

type Group = CatalogKind | "scratch";

export function StartFromSheet({ open, onClose, projectId, onChoose, only }: {
  open: boolean;
  onClose: () => void;
  /** The project whose own agents and skills are listed too; none for a task on its own. */
  projectId: string | null;
  onChoose: (start: LaunchStart, agent: AgentChoice | null) => void;
  /** Agents or skills alone, with no way to the others or the scratch pad. */
  only?: CatalogKind;
}) {
  const online = useLink().state === "online";
  const disabled = useDisabled();
  const harnesses = useHost((s) => s.harnesses);
  const catalog = useCatalog(projectId === ONE_OFF_DRAFT ? null : projectId, open && online);
  const [chosen, setGroup] = useState<Group>("agent");
  const group = only ?? chosen;
  const [query, setQuery] = useState("");
  // What is disabled in Øphio is never offered to start from.
  const listed = group === "scratch" ? [] : (catalog.catalog?.entries ?? []).filter((e) => e.kind === group && (!disabled.supported || !disabled.isDisabled(e.disable?.state === "can" ? e.disable.key ?? e.host_path : e.host_path, listedDisabled(e.disable), e.disable?.state === "can" && !!e.disable.key)));
  const shown = matching(listed, query);
  const noun = group === "agent" ? "agents" : "skills";
  const close = () => { setQuery(""); onClose(); };
  return (
    <Sheet
      open={open}
      onClose={close}
      title={only === "agent" ? "Start from an agent" : only === "skill" ? "Start from a skill" : "Start from"}
      subtitle={only ? "Its instructions start the conversation" : "An agent’s or skill’s instructions, or a clean slate"}
      className="launch-sheet"
      // Not focused at first, so the keyboard comes up only when tapped.
      head={group !== "scratch" && listed.length > 0 && <TextField label={`Search ${noun}`} hideLabel placeholder={`Search ${noun}`} type="search" value={query} onChange={setQuery} enterKeyHint="search" />}
    >
      {!only && (
        <Segmented
          label="Start from"
          value={group}
          onChange={(g) => { setGroup(g); setQuery(""); }}
          options={[
            { value: "agent", label: "Agents" },
            { value: "skill", label: "Skills" },
            { value: "scratch", label: "Scratch" },
          ]}
        />
      )}
      {group === "scratch" ? (
        <div className="group">
          <ListRow icon={NotebookPen} title="Scratch pad" subtitle="No instructions added to what you write." chevron onClick={() => onChoose(SCRATCH, null)} />
        </div>
      ) : catalog.failed ? (
        <Notice tone="warn" title="Couldn’t read the list" action={<Button onClick={catalog.refresh} pending={catalog.loading}>Try again</Button>}>
          {catalog.failed}
        </Notice>
      ) : !catalog.catalog ? (
        online ? <div className="screen__loading" aria-busy="true" aria-label="Reading agents and skills" /> : <p className="fine">Connect to the computer to see its agents and skills.</p>
      ) : listed.length > 0 && shown.length === 0 ? (
        <p className="fine">No {group === "agent" ? "agent" : "skill"} matches “{query.trim()}”.</p>
      ) : shown.length === 0 ? (
        <Empty icon={group === "agent" ? Bot : Sparkles} title={group === "agent" ? "No agents yet" : "No skills found"}>
          {group === "agent" ? "Make one in Library." : "Skills are folders with a SKILL.md in Claude Code’s or Codex’s skills folders."}
        </Empty>
      ) : (
        <div className="group">
          {shown.map((entry) => (
            <ListRow
              key={`${entry.source}/${entry.id}`}
              icon={entry.kind === "agent" ? Bot : Sparkles}
              title={entry.name}
              subtitle={
                <>
                  {entry.description && <span className="launch-row__desc">{entry.description}</span>}
                  <span className="launch-row__from">{SOURCE_LABEL[entry.source]}</span>
                </>
              }
              onClick={() => { setQuery(""); onChoose(launchStart(entry), entryAgent(entry, harnesses)); }}
            />
          ))}
        </div>
      )}
    </Sheet>
  );
}
