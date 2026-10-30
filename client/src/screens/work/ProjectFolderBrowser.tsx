// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { describeError, useApp, useCommand, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { Notice } from "../../design/Notice";
import { PathText } from "../../design/PathText";
import type { ProjectFolder, ProjectFolderListing } from "../../protocol/types";
import { CommandFeedback, useUnconfirmedCommand } from "./shared";
import "./projectFolders.css";

// Folder names are read from the paired computer, never from this device.
export function ProjectFolderBrowser({ onClose, footer }: {
  onClose: () => void;
  footer: HTMLDivElement | null;
}) {
  const { session, profile, ui } = useApp();
  const send = useCommand();
  const link = useLink();
  const admin = useHost(s => s.grants?.admin === true);
  const projects = useHost(s => s.projects);
  const uncertain = useUnconfirmedCommand(c => c.command === "add_project");
  const [path, setPath] = useState<string | null>(null);
  const [after, setAfter] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [hidden, setHidden] = useState(false);
  const [revision, refresh] = useState(0);
  const [listing, setListing] = useState<ProjectFolderListing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<ProjectFolder[]>([]);
  const [review, setReview] = useState(false);
  const [pending, setPending] = useState(false);
  const [outcome, setOutcome] = useState<string | null>(null);
  const active = useRef(false);
  const adding = useRef(false);
  const interrupted = useRef(false);
  const online = link.state === "online";
  const available = online && admin;
  const registered = (f: ProjectFolder) => !!f.project_id || Object.values(projects).some(p => p.host_path === f.path);
  const choices = selected.filter(f => !registered(f));
  const eligible = (listing?.folders ?? []).filter(f => !registered(f) && !f.unavailable);
  const canSelect = available && !pending && !uncertain;

  useEffect(() => {
    active.current = true;
    // Observe disconnects immediately, not only after a React render. A new
    // connection must not silently resume the rest of a batch.
    const check = () => {
      if (!session.online || !session.state.get().grants?.admin) interrupted.current = true;
    };
    const offLink = session.link.subscribe(check);
    const offState = session.state.subscribe(check);
    return () => { active.current = false; interrupted.current = true; offLink(); offState(); };
  }, [session]);

  useEffect(() => {
    let current = true;
    setListing(null);
    setError(null);
    if (!available || review) return;
    // Debounce the filter, and discard answers for locations/filters left behind.
    const timer = setTimeout(() => {
      void session.query({ kind: "project_folders", path, after, show_hidden: hidden, search }).then(
        r => { if (current) setListing(r.listing); },
        e => { if (current) setError(describeError(e, profile.name)); },
      );
    }, 200);
    return () => { current = false; clearTimeout(timer); };
  }, [session, profile.name, available, link, path, after, hidden, search, revision, review]);

  function navigate(next: string | null) {
    setListing(null); setPath(next); setAfter(null); setSearch(""); refresh(n => n + 1);
  }
  function toggle(folder: ProjectFolder) {
    if (!canSelect) return;
    setSelected(old => old.some(f => f.path === folder.path)
      ? old.filter(f => f.path !== folder.path)
      : old.length < 100 ? [...old, folder] : old);
  }
  async function add() {
    if (!canSelect || adding.current || !choices.length) return;
    adding.current = true;
    interrupted.current = false;
    setPending(true); setOutcome(null);
    const batch = [...choices];
    let added = 0;
    let first: string | null = null;
    for (const folder of batch) {
      if (!active.current || interrupted.current || !session.online || !session.state.get().grants?.admin) break;
      if (Object.values(session.state.get().projects).some(p => p.host_path === folder.path)) continue;
      const receipt = await send({ command: "add_project", host_path: folder.path, name: null }, {
        label: `Add ${folder.name}`, toastErrors: false,
      });
      // Unknown results remain in CommandFeedback; never generate a new id and
      // retry them here, or continue adding other folders after an error.
      if (receipt?.result?.kind !== "project_added") break;
      added++;
      first ??= receipt.result.project_id;
      if (!active.current) break;
      setSelected(old => old.filter(f => f.path !== folder.path));
    }
    adding.current = false;
    if (!active.current) return;
    setPending(false);
    if (added === batch.length && first) {
      ui.selectProject(first);
      onClose();
    } else {
      setOutcome(`${added} of ${batch.length} confirmed added. Stopped here; review the result below before adding the remaining folders.`);
    }
  }
  function folderRow(folder: ProjectFolder) {
    const added = registered(folder);
    const checked = selected.some(f => f.path === folder.path);
    return <div className="project-folder" key={folder.path}>
      <label className="project-folder__select">
        <input type="checkbox" aria-label={`Select ${folder.name}`} checked={checked && !added}
          disabled={!canSelect || added || !!folder.unavailable || (!checked && selected.length >= 100)}
          onChange={() => toggle(folder)} />
      </label>
      <button className="project-folder__open" type="button" disabled={!available || pending}
        aria-label={`Open ${folder.name}`} onClick={() => navigate(folder.path)}>
        <span className="project-folder__name">{folder.name}</span>
        <span className="project-folder__detail">{added ? "Already added" : folder.unavailable ?? "Open folder →"}</span>
      </button>
    </div>;
  }
  return <div className="work-stack">
    <p className="work-muted">Folders on <strong>{profile.name}</strong>, not this device. Open a folder to look inside, or select folders to add as projects.</p>
    {!admin && <Notice>Browsing and adding projects need this device’s Manage permission.</Notice>}
    {!online && <Notice>Connect to {profile.name} to browse its folders.</Notice>}
    {review ? <>
      <h3>Review {choices.length} selected {choices.length === 1 ? "folder" : "folders"}</h3>
      <p>Only these folders will be registered. Their files stay where they are. New project queues start paused; no task starts.</p>
      <ul className="project-folder-review">{choices.map(f => <li key={f.path}><strong>{f.name}</strong><PathText path={f.path} /></li>)}</ul>
      <Button disabled={pending} onClick={() => { setReview(false); setOutcome(null); }}>Back to folders</Button>
    </> : <>
      <nav className="project-folder-crumbs" aria-label="Computer folders">
        <Button disabled={!available} onClick={() => navigate(null)}>Locations</Button>
        {listing?.ancestors.map(f => <Button key={f.path} onClick={() => navigate(f.path)}>{f.name}</Button>)}
      </nav>
      {path !== null && <>
        <div className="project-folder-current">
          <PathText path={listing?.current?.path ?? path} />
          {listing?.current && <Button disabled={!canSelect || registered(listing.current) || !!listing.current.unavailable}
            onClick={() => toggle(listing.current!)}>
            {registered(listing.current) ? "Already added" : selected.some(f => f.path === listing.current!.path) ? "Deselect this folder" : "Select this folder"}
          </Button>}
          {listing?.current?.unavailable && <p className="work-muted">{listing.current.unavailable}</p>}
        </div>
        <details className="project-folder-filters">
          <summary>Filter folders{search || hidden ? " · active" : ""}</summary>
          <TextField label="Find a folder here" value={search} maxLength={64} onChange={v => { setListing(null); setSearch(v); setAfter(null); }} />
          <label className="project-folder-hidden"><input type="checkbox" checked={hidden} onChange={e => { setListing(null); setHidden(e.target.checked); setAfter(null); }} />Show hidden folders</label>
        </details>
      </>}
      {available && !listing && !error && <p role="status">Reading folders…</p>}
      {error && <Notice tone="stop" action={<Button onClick={() => refresh(n => n + 1)}>Try again</Button>}>{error}</Notice>}
      {listing && <>
        {path !== null && eligible.length > 0 && <Button disabled={!canSelect || selected.length >= 100} onClick={() => setSelected(old => [...old, ...eligible.filter(f => !old.some(s => s.path === f.path))].slice(0, 100))}>Select folders on this page</Button>}
        <div className="project-folder-list">{listing.folders.map(folderRow)}</div>
        {listing.folders.length === 0 && <p>No folders {search ? "match here" : "here"}. Files and folder shortcuts aren’t shown.</p>}
        <div className="work-actions">
          {after && <Button onClick={() => { setListing(null); setAfter(null); }}>First page</Button>}
          {listing.next_after && <Button onClick={() => { setAfter(listing.next_after); setListing(null); }}>Next folders</Button>}
        </div>
      </>}
      {selected.length >= 100 && <Notice>Add up to 100 folders at a time.</Notice>}
      {choices.length > 0 && <Button onClick={() => setSelected([])} disabled={pending}>Clear selection ({choices.length})</Button>}
    </>}
    {pending && <Notice>Adding folders one at a time. Closing stops the remaining additions; an already-sent request may still finish.</Notice>}
    {outcome && <Notice>{outcome}</Notice>}
    <CommandFeedback match={c => c.command === "add_project"} />
    {footer && createPortal(<Button variant="primary" pending={pending} disabled={!canSelect || choices.length === 0}
      onClick={review ? () => void add() : () => setReview(true)}>
      {review ? `Add ${choices.length} ${choices.length === 1 ? "project" : "projects"}` : `Review selection (${choices.length})`}
    </Button>, footer)}
  </div>;
}
