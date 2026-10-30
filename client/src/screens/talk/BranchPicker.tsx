// SPDX-License-Identifier: Apache-2.0
//
// Choosing the Git branch a new task is for, or naming a new one. The list is
// read from the computer when asked, most recently committed first; choosing
// one only tells the agent, and a new one is made by the agent. All branches,
// the default, names none.

import { GitBranchPlus } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import { useApp } from "../../app/context";
import { branchNameInput, branchNameProblem, changesText } from "../../app/taskBranch";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { ListRow } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import type { BranchListing, TaskBranch } from "../../protocol/types";

/** Show a filter once the list is longer than a screen. */
const FILTER_FROM = 10;

export interface Branches {
  listing: BranchListing | null;
  /** The computer answered, even if the folder isn't a repository. */
  read: boolean;
  loading: boolean;
  failed: boolean;
  refresh: () => void;
}

/** The project's branches as the computer reads them now. Nothing is asked
 * while `enabled` is false. */
export function useBranches(projectId: string, enabled: boolean): Branches {
  const { session } = useApp();
  const [state, setState] = useState<Omit<Branches, "refresh">>({ listing: null, read: false, loading: false, failed: false });
  const [asked, setAsked] = useState(0);
  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    setState((s) => ({ ...s, loading: true, failed: false }));
    session.query({ kind: "project_branches", project_id: projectId }).then(
      (r) => { if (!cancelled) setState({ listing: r.listing, read: true, loading: false, failed: false }); },
      () => { if (!cancelled) setState((s) => ({ ...s, loading: false, failed: true })); },
    );
    return () => { cancelled = true; };
  }, [session, projectId, enabled, asked]);
  return { ...state, refresh: () => setAsked((n) => n + 1) };
}

export function BranchSheet({ open, onClose, branches, chosen, onPick, computer }: {
  open: boolean;
  onClose: () => void;
  branches: Branches;
  chosen: TaskBranch | null;
  onPick: (branch: TaskBranch | null) => void;
  computer: string;
}) {
  const [filter, setFilter] = useState("");
  // The name being typed for a new branch; null while choosing from the list.
  const [naming, setNaming] = useState<string | null>(null);
  useEffect(() => { if (!open) { setFilter(""); setNaming(null); } }, [open]);
  const { listing, read, loading, failed } = branches;
  const all = listing?.branches ?? [];
  const needle = filter.trim().toLowerCase();
  const shown = needle ? all.filter((b) => b.toLowerCase().includes(needle)) : all;
  // A branch carried from another task may since have been deleted or renamed.
  const existing = chosen && !chosen.new ? chosen.name : null;
  const missing = existing !== null && read && listing && !listing.truncated && !all.includes(existing);
  // Only a repository can have a branch made in it.
  const canMake = !(read && !listing);
  // A search that matches no branch exactly can become one.
  const candidate = branchNameInput(filter.trim());
  const offerCandidate = canMake && needle !== "" && !all.includes(candidate) && branchNameProblem(candidate) === null;
  const pick = (branch: TaskBranch | null) => { onPick(branch); onClose(); };
  const from = !read || !listing
    ? "The agent makes it from what’s checked out before changing any file; uncommitted changes come with it."
    : `The agent makes it from ${listing.current ?? "the checked-out commit"} before changing any file${listing.dirty_files ? `, and your ${changesText(listing.dirty_files)} come with it` : ""}.`;
  return (
    <Sheet open={open} onClose={onClose} title={naming === null ? "Branch" : "New branch"} subtitle={`On ${computer}`}>
      {failed && (
        <Notice tone="warn" title="Couldn’t read the branches" action={<Button onClick={branches.refresh} pending={loading}>Try again</Button>}>
          {computer} didn’t answer in time. You can still leave it to All branches{naming === null ? "" : ", or name a new one"}.
        </Notice>
      )}
      {naming !== null ? (
        <NewBranchForm name={naming} setName={setNaming} taken={all} from={from} onBack={() => setNaming(null)} onUse={(name) => pick({ name, new: true })} />
      ) : (
        <div className="branch-sheet">
          {loading && !read && <p className="fine" role="status">Reading branches on {computer}…</p>}
          {read && !listing && <p className="fine">This folder isn’t a Git repository {computer} can read, so there are no branches to choose.</p>}
          {all.length > FILTER_FROM && <TextField label="Find a branch" type="search" value={filter} onChange={setFilter} autoComplete="off" autoCapitalize="none" spellCheck={false} />}
          {canMake && (!needle || offerCandidate) && (
            <div className="group">
              {offerCandidate
                ? <ListRow icon={GitBranchPlus} title={`New branch “${candidate}”`} subtitle="The agent makes it before changing any file." onClick={() => pick({ name: candidate, new: true })} className="branch-sheet__name" />
                : <ListRow icon={GitBranchPlus} title="New branch" subtitle="Name one for the agent to make and work on." chevron onClick={() => setNaming(chosen?.new ? chosen.name : "")} />}
            </div>
          )}
          <div className="group branch-sheet__list" role="radiogroup" aria-label="Branch">
            {!needle && <ListRow title="All branches" subtitle="No branch named. The agent works in the folder as it is." checked={chosen === null} onClick={() => pick(null)} />}
            {!needle && chosen?.new && <ListRow title={chosen.name} subtitle="New. The agent makes it before changing any file." checked onClick={() => pick(chosen)} className="branch-sheet__name" />}
            {missing && <ListRow title={existing} subtitle={`Not on ${computer} now. The agent is still told this name.`} checked onClick={() => pick({ name: existing })} className="branch-sheet__name" />}
            {shown.map((b) => (
              <ListRow
                key={b}
                title={b}
                subtitle={b === listing?.current ? `Checked out now${listing.dirty_files ? ` · ${changesText(listing.dirty_files)}` : ""}` : undefined}
                checked={existing === b}
                onClick={() => pick({ name: b })}
                className="branch-sheet__name"
              />
            ))}
          </div>
          {needle && !shown.length && <p className="fine">No branch matches “{filter.trim()}”.</p>}
          {listing?.truncated && <p className="fine">Showing the {all.length} most recently committed branches.</p>}
          <p className="fine">The agent is told the branch each time it starts or continues. Ophio doesn’t make or switch branches itself; the agent is asked to keep every uncommitted change.</p>
        </div>
      )}
    </Sheet>
  );
}

function NewBranchForm({ name, setName, taken, from, onBack, onUse }: {
  name: string;
  setName: (name: string) => void;
  taken: string[];
  from: string;
  onBack: () => void;
  onUse: (name: string) => void;
}) {
  const problem = branchNameProblem(name) ?? (taken.includes(name) ? `${name} already exists. Go back to choose it from the list.` : null);
  // "feature/" and "v1." are on the way to a name; say so only once they stop.
  const [left, setLeft] = useState(false);
  const shown = name === "" || (!left && /[/.]$/.test(name)) ? null : problem;
  const submit = (e: FormEvent) => { e.preventDefault(); if (!problem) onUse(name); };
  return (
    <form className="branch-sheet__form" onSubmit={submit}>
      <TextField
        label="Branch name"
        value={name}
        onChange={(v) => { setName(branchNameInput(v)); setLeft(false); }}
        onBlur={() => setLeft(true)}
        error={shown}
        hint={from}
        placeholder="feature/rate-limits"
        autoFocus
        autoComplete="off"
        autoCapitalize="none"
        spellCheck={false}
        enterKeyHint="done"
      />
      <div className="branch-sheet__actions">
        <Button type="submit" variant="primary" disabled={problem !== null}>Use new branch</Button>
        <Button type="button" variant="quiet" onClick={onBack}>Back to the list</Button>
      </div>
    </form>
  );
}
