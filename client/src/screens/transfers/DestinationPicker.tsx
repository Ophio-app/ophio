// SPDX-License-Identifier: Apache-2.0
import { useId, useState } from "react";
import { Folder as FolderIcon, FolderGit2, FolderPlus } from "lucide-react";
import type { Project, TransferDestination } from "../../protocol/types";
import { shallowEqual, useApp, useHost } from "../../app/context";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { Empty, ListRow } from "../../design/ListRow";
import { PathText } from "../../design/PathText";
import { Sheet } from "../../design/Sheet";
import { Notice } from "../../design/Notice";
import { PROGRAM_NAME } from "../../brand";
import { matchingProjects } from "../../shell/ProjectSheet";
import { relativeDir } from "../library/model";
import { useFolder } from "../library/shared";
import "./transfers.css";

/** The projects a file can be sent into, by name, those started on their own
 * last. Home is never one: the computer refuses its files to and from the
 * phone, and the Inbox is the place for what belongs to no project. */
export function sendableProjects(projects: Record<string, Project>): Project[] {
  return Object.values(projects)
    .filter((p) => !p.home)
    .sort((a, b) => Number(!!a.one_off) - Number(!!b.one_off) || a.name.localeCompare(b.name));
}

export function DestinationPicker({
  value,
  onChange,
  disabled,
}: {
  value: TransferDestination;
  onChange: (value: TransferDestination) => void;
  disabled?: boolean;
}) {
  const destinationGroup = useId();
  const { profile } = useApp();
  const inbox = useHost(s => s.host?.inbox_path);
  // A computer that has reported itself without a folder can't receive into one.
  const noInbox = useHost(s => !!s.host && !s.host.inbox_path);
  const projects = useHost((s) => s.projects),
    sendable = useHost((s) => sendableProjects(s.projects), shallowEqual),
    roots = useHost((s) => s.libraryRoots),
    [browse, setBrowse] = useState(false),
    [choosing, setChoosing] = useState(false);
  const project =
    value.kind === "project_dir" ? projects[value.project_id] : null;
  const root = project
    ? roots.find(
        (r) =>
          r.project_id === project.project_id &&
          !r.only &&
          r.host_path.replace(/[\\/]$/, "") ===
            project.host_path.replace(/[\\/]$/, ""),
      )
    : null;
  return (
    <fieldset className="transfer-destination" disabled={disabled}>
      <legend>Destination</legend>
      {/* A draft saved while Attach to a message was offered: messages can't
          attach these uploads, so none of the choices below is selected. */}
      {value.kind === "artifact" && (
        <p className="transfer-meta">
          Files sent from here can’t be attached to a message. Choose the Inbox
          or a project folder instead.
        </p>
      )}
      <label className="transfer-choice">
        <input
          type="radio"
          name={destinationGroup}
          checked={value.kind === "inbox"}
          disabled={noInbox}
          onChange={() => onChange({ kind: "inbox" })}
        />
        <span>{inbox ? `${inbox} on ${profile.name}` : noInbox ? `Inbox on ${profile.name} · not set up` : "Inbox on the computer"}</span>
      </label>
      {noInbox && (
        <p className="transfer-meta">
          {profile.name} has no folder for received files. Choose another
          destination, or set one on the computer with{" "}
          <code>{PROGRAM_NAME} inbox set FOLDER</code>.
        </p>
      )}
      <label className="transfer-choice">
        <input
          type="radio"
          name={destinationGroup}
          checked={value.kind === "project_dir"}
          disabled={!sendable.length}
          onChange={() => {
            const first = sendable[0];
            if (first)
              onChange({
                kind: "project_dir",
                project_id: first.project_id,
                dir: "",
              });
          }}
        />
        <span>A project folder</span>
      </label>
      {value.kind === "project_dir" && (
        <div className="transfer-stack">
          <div className="group">
            <ListRow
              icon={project?.one_off ? FolderPlus : FolderGit2}
              title={project?.name ?? "Choose a project"}
              ariaLabel={`Project${project ? `: ${project.name}` : ""}. Choose another`}
              chevron
              onClick={() => setChoosing(true)}
            />
          </div>
          <TextField
            label="Folder within project"
            value={value.dir}
            onChange={(dir) => onChange({ ...value, dir })}
            error={relativeDir(value.dir)}
            hint="Leave empty for the project folder. Use / between folder names."
          />
          {project && <p className="transfer-path mono">{project.host_path}</p>}
          {root ? (
            <Button icon={FolderIcon} onClick={() => setBrowse(true)}>
              Choose folder…
            </Button>
          ) : (
            <p className="transfer-meta">
              Folder browsing isn’t shared for this project. Enter a relative
              folder above.
            </p>
          )}
        </div>
      )}
      {choosing && (
        <ProjectChooser
          projects={sendable}
          current={value.kind === "project_dir" ? value.project_id : null}
          onClose={() => setChoosing(false)}
          onChoose={(project_id) => {
            onChange({ kind: "project_dir", project_id, dir: "" });
            setChoosing(false);
          }}
        />
      )}
      {browse && root && value.kind === "project_dir" && (
        <FolderPicker
          rootId={root.root_id}
          initial={value.dir}
          onClose={() => setBrowse(false)}
          onChoose={(dir) => {
            onChange({ ...value, dir });
            setBrowse(false);
          }}
        />
      )}
    </fieldset>
  );
}
/** The projects as a list a search narrows, as the header's own project list
 * is. The field isn't focused on opening, so the keyboard waits for a tap. */
function ProjectChooser({
  projects,
  current,
  onChoose,
  onClose,
}: {
  projects: Project[];
  current: string | null;
  onChoose: (projectId: string) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const shown = matchingProjects(projects, query);
  return (
    <Sheet
      className="transfer-sheet"
      open
      title="Send to a project"
      size="tall"
      onClose={onClose}
      head={
        <TextField
          label="Search projects"
          hideLabel
          placeholder="Search projects"
          type="search"
          value={query}
          onChange={setQuery}
          enterKeyHint="search"
        />
      }
    >
      {shown.length ? (
        <div className="group">
          {shown.map((p) => (
            <ListRow
              key={p.project_id}
              icon={p.one_off ? FolderPlus : FolderGit2}
              title={p.name}
              subtitle={<PathText path={p.host_path} />}
              checked={p.project_id === current}
              onClick={() => onChoose(p.project_id)}
            />
          ))}
        </div>
      ) : (
        <Empty title="No matching projects">Search by a project’s name or folder.</Empty>
      )}
    </Sheet>
  );
}
function FolderPicker({
  rootId,
  initial,
  onChoose,
  onClose,
}: {
  rootId: string;
  initial: string;
  onChoose: (dir: string) => void;
  onClose: () => void;
}) {
  const [path, setPath] = useState(relativeDir(initial) ? "" : initial),
    q = useFolder(rootId, path);
  return (
    <Sheet
      className="transfer-sheet"
      open
      title="Choose a project folder"
      subtitle={path || "Project folder"}
      onClose={onClose}
      footer={
        <Button block variant="primary" onClick={() => onChoose(path)}>
          Use this folder
        </Button>
      }
    >
      <div className="transfer-stack">
        {path && (
          <Button
            onClick={() =>
              setPath(
                path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "",
              )
            }
          >
            Parent folder
          </Button>
        )}
        {q.error && <Notice tone="stop">{q.error}</Notice>}
        {q.loading && <p>Loading folders…</p>}
        {q.cached?.value.entries
          .filter((e) => e.kind === "directory" && !e.hidden)
          .map((e) => (
            <ListRow
              key={e.path}
              icon={FolderIcon}
              title={e.name}
              onClick={() => setPath(e.path)}
              chevron
            />
          ))}
        {q.cached &&
          !q.cached.value.entries.some(
            (e) => e.kind === "directory" && !e.hidden,
          ) && <p className="transfer-meta">No subfolders here.</p>}
      </div>
    </Sheet>
  );
}
