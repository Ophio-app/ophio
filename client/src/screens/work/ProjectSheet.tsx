// SPDX-License-Identifier: Apache-2.0
import { useId, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import {
  shallowEqual,
  useApp,
  useCommand,
  useHost,
  useLink,
} from "../../app/context";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { ListRow, Section } from "../../design/ListRow";
import { CapabilityNote, Notice, availabilityText } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import type { HarnessInfo, HarnessKind, Project } from "../../protocol/types";
import { hostPathError } from "./logic";
import { useLocalText } from "./local";
import {
  CommandFeedback,
  ConnectionNote,
  useUnconfirmedCommand,
} from "./shared";
import { effortLabel } from "../talk/intent";
import { PathText } from "../../design/PathText";
import { ProjectFolderBrowser } from "./ProjectFolderBrowser";
import { FolderOpen } from "lucide-react";
import { oneOffFolder } from "../../app/launchers";
import { DeleteOneOff, useDeleteOneOff } from "./DeleteOneOff";
import { useWarning } from "../../app/warnings";
import { useGithubLinkable } from "../github/GithubDoors";
import { GithubSettingsRow } from "../github/GithubSettings";
import "./work.css";

function selectable(h: HarnessInfo) {
  return h.installed.state === "available" && h.account.state === "signed_in";
}
export default function ProjectSheet({
  project,
  onClose,
}: {
  project: Project | undefined;
  onClose: () => void;
}) {
  const { ui } = useApp();
  const others = useHost(
    (s) => Object.values(s.projects).filter((p) => p.project_id !== project?.project_id),
    shallowEqual,
  );
  const [footer, setFooter] = useState<HTMLDivElement | null>(null);
  const [mode, setMode] = useState<"settings" | "add" | "remove" | "delete">(
    project ? "settings" : "add",
  );
  const deleteWarning = useWarning("project.delete");
  const removeWarning = useWarning("project.remove");
  const deleteOneOff = useDeleteOneOff();
  const removeProject = useRemoveProject(onClose);
  const deleted = () => {
    const next = others.find((p) => !p.one_off) ?? others[0];
    if (next) ui.selectProject(next.project_id);
    onClose();
  };
  return (
    <Sheet
      open
      onClose={onClose}
      className="work-sheet"
      title={
        mode === "add"
          ? "Add project"
          : mode === "remove"
            ? "Remove project"
            : mode === "delete"
              ? "Delete one-off"
              : "Project settings"
      }
      subtitle={mode === "add" ? undefined : project?.name}
      footer={<div className="work-sheet-primary" ref={setFooter} />}
    >
      <ConnectionNote />
      {mode === "add" ? (
        <AddProject onClose={onClose} footer={footer} />
      ) : mode === "remove" && project ? (
        <RemoveProject
          project={project}
          onClose={onClose}
          onCancel={() => setMode("settings")}
          footer={footer}
        />
      ) : mode === "delete" && project ? (
        <DeleteOneOff
          project={project}
          onCancel={() => setMode("settings")}
          onDeleted={deleted}
          footer={footer}
        />
      ) : project ? (
        <>
          {project.one_off && (
            <OneOff project={project} onClose={onClose} more={deleteWarning.more} pending={deleteOneOff.pending}
              onDelete={() => deleteWarning.ask(() => setMode("delete"), () => void deleteOneOff.run(project, deleted))} />
          )}
          <ListRow title="Project stages" subtitle="Review stage decisions and history" chevron onClick={() => {
            onClose(); ui.push({ screen: "stages", projectId: project.project_id });
          }} />
          <ListRow title="Notifications" subtitle="What alerts you from this project" chevron onClick={() => {
            onClose(); ui.push({ screen: "settings", section: "settings-notifications", projectId: project.project_id });
          }} />
          <GithubRow project={project} />
          <Defaults
            key={project.project_id}
            project={project}
            footer={footer}
          />
          <Section title="Projects">
            <div className="group">
              <ListRow
                title="Add project"
                subtitle="Choose a folder on the computer"
                chevron
                onClick={() => setMode("add")}
              />
              <ListRow
                title={`Remove ${project.name}${removeWarning.more}`}
                subtitle="Keep its files and task history"
                chevron={removeWarning.more !== ""}
                onClick={() => removeWarning.ask(() => setMode("remove"), () => void removeProject.run(project))}
              />
            </div>
          </Section>
        </>
      ) : null}
    </Sheet>
  );
}
/** A task started on its own: its folder, making it a project of its own,
 * and deleting it. */
/** Turning GitHub on or off, for a project that can use it; its sheet opens over this one. */
function GithubRow({ project }: { project: Project }) {
  return useGithubLinkable(project) ? <GithubSettingsRow project={project} /> : null;
}

function OneOff({ project, onClose, onDelete, more, pending: deleting }: {
  project: Project; onClose: () => void; onDelete: () => void;
  /** The “…” Delete ends with while it still opens its warning. */
  more: string; pending: boolean;
}) {
  const { ui, profile } = useApp();
  const send = useCommand();
  const online = useLink().state === "online";
  const admin = useHost((s) => s.grants?.admin === true);
  const roots = useHost((s) => s.libraryRoots);
  const folder = oneOffFolder(project, roots);
  const uncertain = useUnconfirmedCommand((c) => c.command === "promote_one_off" && c.project_id === project.project_id);
  const [pending, setPending] = useState(false);
  return (
    <Section title="One-off">
      <p className="fine">Started on its own, so it’s listed under One-offs rather than among your projects.</p>
      <div className="group">
        {folder && (
          <ListRow
            icon={FolderOpen}
            title="Open its folder"
            subtitle="What it made is in Library"
            chevron
            onClick={() => { onClose(); ui.push({ screen: "folder", rootId: folder.rootId, path: folder.path }, "library"); }}
          />
        )}
      </div>
      <div className="work-actions">
        <Button
          disabled={!online || !admin || uncertain}
          pending={pending}
          onClick={async () => {
            setPending(true);
            await send({ command: "promote_one_off", project_id: project.project_id }, { label: `Make ${project.name} a project` });
            setPending(false);
          }}
        >
          Make it a project
        </Button>
        <Button variant="stop-outline" disabled={!admin} pending={deleting} onClick={onDelete}>
          Delete it{more}
        </Button>
      </div>
      {!admin && <p className="fine">Only a device with admin access on {profile.name} can do this.</p>}
      <CommandFeedback match={(c) => c.command === "promote_one_off" && c.project_id === project.project_id} />
    </Section>
  );
}
function PrimaryAction({
  target,
  children,
}: {
  target: HTMLDivElement | null;
  children: ReactNode;
}) {
  return target ? createPortal(children, target) : null;
}
function Defaults({
  project,
  footer,
}: {
  project: Project;
  footer: HTMLDivElement | null;
}) {
  const { profile } = useApp();
  const send = useCommand();
  const link = useLink();
  const catalog = useHost((s) => s.harnesses);
  const unavailable = catalog.filter((h) => !selectable(h));
  const admin = useHost((s) => s.grants?.admin === true);
  const uncertain = useUnconfirmedCommand(
    (c) =>
      c.command === "update_project_defaults" &&
      c.project_id === project.project_id,
  );
  const key = `${profile.profileId}.defaults.${project.project_id}`;
  const [kind, setKind, saved] = useLocalText(
    `${key}.agent`,
    project.default_harness ?? "",
  );
  const [model, setModel, modelSaved] = useLocalText(
    `${key}.model`,
    project.default_model ?? "",
  );
  const [effort, setEffort, effortSaved] = useLocalText(
    `${key}.effort`,
    project.default_effort ?? "",
  );
  const [pending, setPending] = useState(false);
  const h = catalog.find((h) => h.kind === kind);
  const modelAllowed = h?.capabilities.model_selection.state === "available";
  const effortAllowed = h?.capabilities.effort_selection.state === "available";
  return (
    <div className="work-stack">
      <dl className="work-facts">
        <div>
          <dt>Name</dt>
          <dd>{project.name}</dd>
        </div>
        <div>
          <dt>On computer</dt>
          <dd className="mono"><PathText path={project.host_path} /></dd>
        </div>
      </dl>
      <Section title="Defaults for new tasks">
        <label className="field">
          <span className="field__label">Agent</span>
          <select
            className="field__input"
            value={kind}
            onChange={(e) => {
              setKind(e.target.value);
              setModel("");
              setEffort("");
            }}
          >
            <option value="">Use the computer’s default</option>
            {catalog.map((h) => (
              <option key={h.kind} value={h.kind} disabled={!selectable(h)}>
                {h.display_name}
                {!selectable(h) ? " — unavailable" : ""}
              </option>
            ))}
          </select>
        </label>
        {/* Why an agent can't be chosen is a tap away, not a wall of notices. */}
        {unavailable.length > 0 && (
          <details className="work-details">
            <summary>
              Why {unavailable.length === 1 ? `${unavailable[0]!.display_name} is` : `${unavailable.length} agents are`} unavailable
            </summary>
            {unavailable.map((h) => (
              <Notice key={h.kind} title={h.display_name}>
                {h.installed.state !== "available"
                  ? availabilityText(h.installed).detail
                  : h.account.state === "signed_out"
                    ? h.account.remedy
                    : h.account.state === "unknown"
                      ? h.account.reason
                      : "Unavailable"}
              </Notice>
            ))}
          </details>
        )}
        {h && (
          <>
            {modelAllowed ? (
              <label className="field">
                <span className="field__label">Model</span>
                <select
                  className="field__input"
                  value={model}
                  onChange={(e) => setModel(e.target.value)}
                >
                  <option value="">Agent default</option>
                  {h.models.map((m) => (
                    <option key={m.id} value={m.id}>
                      {m.label}
                    </option>
                  ))}
                </select>
              </label>
            ) : (
              <CapabilityNote
                what="Model selection"
                availability={h.capabilities.model_selection}
              />
            )}
            {effortAllowed ? (
              <label className="field">
                <span className="field__label">Effort</span>
                <select
                  className="field__input"
                  value={effort}
                  onChange={(e) => setEffort(e.target.value)}
                >
                  <option value="">Agent default</option>
                  {h.efforts.map((e) => (
                    <option key={e} value={e}>
                      {effortLabel(e)}
                    </option>
                  ))}
                </select>
              </label>
            ) : (
              <CapabilityNote
                what="Effort selection"
                availability={h.capabilities.effort_selection}
              />
            )}
          </>
        )}
        {(!saved || !modelSaved || !effortSaved) && (
          <Notice tone="stop">
            These choices haven’t been saved on this device. Keep this sheet open.
          </Notice>
        )}
        {!admin && (
          <Notice>This device needs the Manage access permission to change project settings.</Notice>
        )}
        <PrimaryAction target={footer}>
          <Button
            variant="primary"
            pending={pending}
            disabled={
              uncertain ||
              link.state !== "online" ||
              !admin ||
              (!!kind && (!h || !selectable(h))) ||
              (!!model &&
                modelAllowed &&
                !h?.models.some((m) => m.id === model)) ||
              (!!effort && effortAllowed && !h?.efforts.includes(effort))
            }
            onClick={async () => {
              setPending(true);
              await send(
                {
                  command: "update_project_defaults",
                  project_id: project.project_id,
                  harness: (h?.kind ?? null) as HarnessKind | null,
                  model: modelAllowed ? model || null : null,
                  effort: effortAllowed ? effort || null : null,
                },
                { toastErrors: false, label: "Save project defaults" },
              );
              setPending(false);
            }}
          >
            Save defaults
          </Button>
        <CommandFeedback
          limit={1}
          match={(c) =>
            c.command === "update_project_defaults" &&
            c.project_id === project.project_id
          }
        />
        </PrimaryAction>
      </Section>
    </div>
  );
}
function AddProject(props: { onClose: () => void; footer: HTMLDivElement | null }) {
  const { profile } = useApp();
  const supported = useHost(s => s.capabilities?.features.includes("project_folders") === true);
  const [manual, setManual] = useState(false);
  const uncertain = useUnconfirmedCommand(c => c.command === "add_project");
  return <div className="work-stack">
    {supported && !manual ? <ProjectFolderBrowser key={profile.profileId} {...props} /> : <>
      {!supported && <Notice>Update Ophio on the computer to browse folders. You can still enter a path below.</Notice>}
      <ManualAddProject key={profile.profileId} {...props} />
    </>}
    {supported && <Button disabled={uncertain} onClick={() => setManual(v => !v)}>{manual ? "Browse computer folders" : "Enter a path instead"}</Button>}
  </div>;
}
/** Shared path form for first-run setup and the project sheets. */
export function ManualAddProject({
  onClose,
  footer,
}: {
  onClose?: () => void;
  footer?: HTMLDivElement | null;
}) {
  const formId = useId();
  const { profile, ui } = useApp();
  const send = useCommand();
  const link = useLink();
  const host = useHost((s) => s.host);
  const windows = useHost((s) => s.capabilities?.os === "windows");
  const admin = useHost((s) => s.grants?.admin === true);
  const [path, setPath, saved] = useLocalText(
    `${profile.profileId}.project-path`,
  );
  const [name, setName, nameSaved] = useLocalText(
    `${profile.profileId}.project-name`,
  );
  const [attempted, setAttempted] = useState(false);
  const [pending, setPending] = useState(false);
  const uncertain = useUnconfirmedCommand((c) => c.command === "add_project");
  const error = hostPathError(path, host?.os_label ?? null);
  const submit = (
    <Button
      type="submit"
      form={formId}
      variant="primary"
      block={footer === undefined}
      pending={pending}
      disabled={link.state !== "online" || !admin || uncertain}
    >
      Add project
    </Button>
  );
  return (
    <form
      id={formId}
      className="work-stack"
      onSubmit={async (e) => {
        e.preventDefault();
        setAttempted(true);
        if (error || pending || uncertain || link.state !== "online" || !admin)
          return;
        setPending(true);
        const r = await send(
          {
            command: "add_project",
            host_path: path,
            name: name.trim() || null,
          },
          { toastErrors: false, label: "Add project" },
        );
        setPending(false);
        if (r?.result?.kind === "project_added") {
          ui.selectProject(r.result.project_id);
          setPath("");
          setName("");
          onClose?.();
        }
      }}
    >
      <p className="work-muted">
        A project is a folder on the computer that agents may work in. Nothing
        outside it is offered to them. Enter the path as it appears on {profile.name}.
      </p>
      <TextField
        label="Folder on computer"
        value={path}
        onChange={setPath}
        error={attempted ? error : null}
        placeholder={windows ? "C:\\Users\\you\\code\\project" : "/home/you/code/project"}
        autoCapitalize="off"
        autoCorrect="off"
        spellCheck={false}
        className="mono-field"
      />
      <TextField
        label="Project name (optional)"
        value={name}
        onChange={setName}
        placeholder="Uses the folder name"
        maxLength={120}
      />
      {(!saved || !nameSaved) && (
        <Notice tone="stop">Not saved on this device. Keep this {footer === undefined ? "screen" : "sheet"} open.</Notice>
      )}
      {!admin && <Notice>Adding projects needs this device’s Manage permission.</Notice>}
      {footer === undefined ? submit : <PrimaryAction target={footer}>{submit}</PrimaryAction>}
      <CommandFeedback match={(c) => c.command === "add_project"} />
    </form>
  );
}
/** Removes a project from the app's list, choosing another, then closes. */
function useRemoveProject(onClose: () => void) {
  const { ui, session } = useApp();
  const send = useCommand();
  const [pending, setPending] = useState(false);
  const run = async (project: Project) => {
    setPending(true);
    const r = await send(
      { command: "remove_project", project_id: project.project_id },
      { toastErrors: false, label: `Remove ${project.name}` },
    );
    setPending(false);
    if (r) {
      const next = Object.values(session.state.get().projects).find((p) => p.project_id !== project.project_id);
      if (next) ui.selectProject(next.project_id);
      onClose();
    }
  };
  return { run, pending };
}
function RemoveProject({
  project,
  onClose,
  onCancel,
  footer,
}: {
  project: Project;
  onClose: () => void;
  onCancel: () => void;
  footer: HTMLDivElement | null;
}) {
  const link = useLink();
  const admin = useHost((s) => s.grants?.admin === true);
  const uncertain = useUnconfirmedCommand(
    (c) =>
      c.command === "remove_project" && c.project_id === project.project_id,
  );
  const remove = useRemoveProject(onClose);
  const warn = useWarning("project.remove");
  return (
    <div className="work-stack">
      <p>
        Remove <strong>{project.name}</strong> from the app’s projects? Task
        history stays on the computer. Nothing on disk is deleted.
      </p>
      {warn.check}
      <PrimaryAction target={footer}>
        <div className="work-actions">
          <Button onClick={onCancel}>Keep project</Button>
          <Button
            variant="stop"
            disabled={link.state !== "online" || !admin || uncertain}
            pending={remove.pending}
            onClick={() => { warn.agreed(); void remove.run(project); }}
          >
            Remove {project.name}
          </Button>
        </div>
      </PrimaryAction>
      <CommandFeedback
        match={(c) =>
          c.command === "remove_project" && c.project_id === project.project_id
        }
      />
    </div>
  );
}
