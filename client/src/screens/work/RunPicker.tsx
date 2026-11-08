// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { ListRow } from "../../design/ListRow";
import { Sheet } from "../../design/Sheet";
import type { Run, Task, TaskMode } from "../../protocol/types";
import { decisionLabel, runMode } from "./logic";
import { modeLabel } from "../talk/ModePicker";
import { runEnd } from "../talk/timeline";

/** What a run is called by its mode: a chat's turns are the chat's, never
 * Ask, which is only how the computer runs them. A task a chat was turned
 * into keeps its chat turns, which ran in Ask before it moved to Work. */
function modeWord(task: Task, mode: TaskMode | null): string | null {
  if (task.conversation) return null;
  if (task.access && mode === "ask") return "Chat";
  return mode ? modeLabel(mode) : null;
}

/** "Run 1 · Plan · Plan proposed" with its start, and the same facts as a spoken label. */
function describe(task: Task, runs: Run[], run: Run, latest: Run | undefined) {
  const mode = runMode(task, run);
  const word = modeWord(task, mode);
  const facts = [`Run ${runs.length - runs.findIndex(r => r.run_id === run.run_id)}`, ...(word ? [word] : []), decisionLabel(run) ?? runEnd(run.state, mode, !!task.conversation)];
  const at = new Date(run.admitted_at).toLocaleString([], { month: "short", day: "numeric", year: "numeric", hour: "numeric", minute: "2-digit" });
  const isLatest = run.run_id === latest?.run_id;
  return {
    title: facts.join(" · "),
    subtitle: `Started ${at}${isLatest ? " · Latest" : ""}`,
    spoken: [...facts, `started ${at}`, ...(isLatest ? ["latest"] : [])].join(", "),
  };
}

export function RunPicker({ task, runs, latest, selectedId, onChange }: {
  task: Task;
  runs: Run[];
  latest: Run | undefined;
  selectedId: string | null;
  onChange: (id: string | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const selected = selectedId ? runs.find(r => r.run_id === selectedId) : latest;
  const shown = selected && describe(task, runs, selected, latest);
  const missing = selectedId ? "Earlier run unavailable" : "Latest run";
  const choose = (id: string | null) => { onChange(id); setOpen(false); };
  return <>
    <div className="group">
      <ListRow ariaLabel={`Choose a run: ${shown ? `showing ${shown.spoken}` : missing}`} title={shown?.title ?? missing} subtitle={shown?.subtitle ?? "Choose an available run"} chevron onClick={() => setOpen(true)} />
    </div>
    <Sheet open={open} onClose={() => setOpen(false)} title="Choose a run">
      <div className="group">
        {runs.map(run => {
          const line = describe(task, runs, run, latest);
          return <ListRow key={run.run_id} ariaLabel={line.spoken} title={line.title} subtitle={line.subtitle} selected={selected?.run_id === run.run_id} onClick={() => choose(run.run_id === latest?.run_id ? null : run.run_id)} />;
        })}
      </div>
    </Sheet>
  </>;
}
