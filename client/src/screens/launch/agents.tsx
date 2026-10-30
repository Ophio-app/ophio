// SPDX-License-Identifier: Apache-2.0
//
// Agents, skills and the scratch pad, as the computer finds them now, for
// Library to list. Choosing one opens the same Review a new task has, already
// set to start from it; nothing about the entry is kept on the phone beyond
// that draft.

import { useEffect, useRef, useState, type ReactNode } from "react";
import { FileText, Pencil, Sparkles } from "lucide-react";
import { describeError, shallowEqual, useApp, useCommand, useHost, useLink, useUi } from "../../app/context";
import { LAUNCHERS_FEATURE, LAUNCHERS_UPDATE, ONE_OFF_DRAFT, SCRATCH, SOURCE_LABEL, entryAgent, launchStart, setDraftStart, useCatalog, type LaunchStart } from "../../app/launchers";
import type { AgentChoice } from "../../app/ui";
import { Button } from "../../design/Button";
import { TextArea, TextField } from "../../design/Field";
import { Empty } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { PathText } from "../../design/PathText";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import { commandResult } from "../../host/commandResult";
import type { CatalogEntry, CatalogFile, CommandReceipt, CommandResult, HarnessInfo } from "../../protocol/types";
import { useRoute } from "../../shell/Screen";
import { modelName } from "../talk/catalog";
import { effortLabel } from "../talk/intent";
import { listedDisabled, useDisabled } from "../../app/disabled";
import { Checking, DisableControl } from "../library/Disabled";
import "./launch.css";

export interface Agents {
  /** Everything the computer lists, before any search. */
  agents: CatalogEntry[];
  skills: CatalogEntry[];
  harnesses: HarnessInfo[];
  canMake: boolean;
  online: boolean;
  start: (entry: CatalogEntry) => void;
  about: (entry: CatalogEntry) => void;
  scratch: () => void;
  make: () => void;
  /** In place of the lists while there are none to show: an older computer,
   * a device not allowed tasks, no connection yet, or still reading. */
  blocked: ReactNode;
  /** Before the lists: the last read failed. */
  failed: ReactNode;
  /** After the lists: what couldn't be read, and that there are more. */
  problems: ReactNode;
  /** What an agent and a skill are, for a list with none. */
  none: { agent: string; skill: string };
  /** Reading the list again, with the last one still shown. */
  refreshing: boolean;
  /** The About and New agent sheets, rendered once by the screen. */
  sheets: ReactNode;
}

/** What `query` leaves of `entries`, by name or description. */
export function matching(entries: CatalogEntry[], query: string): CatalogEntry[] {
  const q = query.trim().toLowerCase();
  return q ? entries.filter((e) => e.name.toLowerCase().includes(q) || !!e.description?.toLowerCase().includes(q)) : entries;
}

/** The agent, model and effort an entry asks for, by the names a person reads:
 * an agent file names its model as Claude Code does, "opus", and its effort "high". */
export function entryAsks(entry: CatalogEntry, harnesses: HarnessInfo[]): { agent: string | null; model: string | null; effort: string | null } {
  const harness = harnesses.find((h) => h.kind === entry.harness);
  const agent = entry.harness ? harness?.display_name ?? entry.harness : null;
  return {
    agent,
    model: entry.model ? modelName(entry.model, harness, agent ?? undefined) : null,
    effort: entry.effort ? `${effortLabel(entry.effort)} effort` : null,
  };
}

/** "Claude Code · Opus (latest) · High effort", what an entry asks for unless changed in Review. */
function defaults(entry: CatalogEntry, harnesses: HarnessInfo[]): string | null {
  const { agent, model, effort } = entryAsks(entry, harnesses);
  const parts = [agent, model, effort].filter(Boolean);
  return parts.length ? parts.join(" · ") : null;
}

export function useAgents(): Agents {
  const { ui, profile } = useApp();
  const online = useLink().state === "online";
  const known = useHost((s) => s.capabilities !== null && s.capabilities !== undefined);
  const supported = useHost((s) => !!s.capabilities?.features.includes(LAUNCHERS_FEATURE));
  const grants = useHost((s) => s.grants);
  const harnesses = useHost((s) => s.harnesses);
  const selected = useUi((s) => s.projectId);
  const project = useHost((s) => (selected ? s.projects[selected] : undefined), shallowEqual);
  const catalog = useCatalog(project?.project_id ?? null, online && supported && !!grants?.tasks);
  const [open, setOpen] = useState<CatalogEntry | null>(null);
  // Back from editing one of their files, the list shows it as it is now.
  const { active } = useRoute();
  const wasActive = useRef(active);
  const { refresh } = catalog;
  useEffect(() => {
    if (active && !wasActive.current) refresh();
    wasActive.current = active;
  }, [active, refresh]);
  const [making, setMaking] = useState(false);

  /** Set the new-task draft to start from `start`, then review it. */
  const launch = (start: LaunchStart, agent: AgentChoice | null) => {
    // With no project chosen, the draft is one with nowhere to be but its own folder.
    const projectId = project?.project_id ?? ONE_OFF_DRAFT;
    setDraftStart(ui, projectId, start, agent);
    ui.push({ screen: "review", projectId, taskId: null, intent: "new_task" });
  };
  const start = (entry: CatalogEntry) => launch(launchStart(entry), entryAgent(entry, harnesses));

  const entries = catalog.catalog?.entries ?? [];
  const problems = catalog.catalog?.problems ?? [];
  return {
    agents: entries.filter((e) => e.kind === "agent"),
    skills: entries.filter((e) => e.kind === "skill"),
    harnesses,
    canMake: !!grants?.library_write,
    online,
    start,
    about: setOpen,
    scratch: () => launch(SCRATCH, null),
    make: () => setMaking(true),
    blocked:
      known && !supported ? (
        <Empty icon={Sparkles} title="Not on this computer yet">{LAUNCHERS_UPDATE}</Empty>
      ) : !grants?.tasks ? (
        <Empty icon={Sparkles} title="Not allowed on this device">
          This device isn’t allowed to work with tasks on {profile.name}.
        </Empty>
      ) : !online && !catalog.catalog ? (
        <Empty icon={Sparkles} title={`Connect to ${profile.name}`}>
          Agents and skills are read from the computer each time, so they show once it’s connected.
        </Empty>
      ) : !catalog.catalog && !catalog.failed ? (
        <Checking />
      ) : null,
    refreshing: catalog.loading && !!catalog.catalog,
    failed: catalog.failed && (
      <Notice tone="warn" title="Couldn’t read the list" action={<Button onClick={catalog.refresh} pending={catalog.loading}>Try again</Button>}>
        {catalog.failed}
      </Notice>
    ),
    problems: (
      <>
        {catalog.catalog?.truncated && <p className="fine">There are more than can be listed here. The rest are on {profile.name}.</p>}
        {problems.length > 0 && (
          <details className="launch-problems">
            <summary>{problems.length === 1 ? "1 couldn’t be read" : `${problems.length} couldn’t be read`}</summary>
            <ul>{problems.map((p) => <li key={p}>{p}</li>)}</ul>
          </details>
        )}
      </>
    ),
    none: {
      agent: `Agents are Markdown files with a name, a description and instructions, kept in Claude Code’s agents folders or Øphio’s own on ${profile.name}.`,
      skill: `Skills are folders with a SKILL.md, in Claude Code’s or Codex’s skills folders on ${profile.name}.`,
    },
    sheets: (
      <>
        <EntrySheet entry={open} onClose={() => setOpen(null)} harnesses={harnesses} canEdit={!!grants?.library_write} onStart={(entry) => { setOpen(null); start(entry); }} onChanged={catalog.refresh} />
        <NewAgentSheet open={making} onClose={() => setMaking(false)} onMade={catalog.refresh} />
      </>
    ),
  };
}

/**
 * What a command that writes a file made. The computer accepts it first and
 * says what it made once it has, so this waits for that; what goes wrong, or
 * an outcome it can't confirm, is said in a toast.
 */
function useSettled() {
  const { session, profile } = useApp();
  return async <K extends CommandResult["kind"]>(receipt: CommandReceipt | null, kind: K): Promise<Extract<CommandResult, { kind: K }> | null> => {
    if (!receipt) return null;
    try {
      const result = await commandResult(session, receipt);
      if (result?.kind === kind) return result as Extract<CommandResult, { kind: K }>;
      toast(`${profile.name} hasn’t said what it made. Check Library before trying again.`, { tone: "stop" });
    } catch (e) {
      toast(describeError(e, profile.name), { tone: "stop" });
    }
    return null;
  };
}

function EntrySheet({ entry, onClose, harnesses, canEdit, onStart, onChanged }: { entry: CatalogEntry | null; onClose: () => void; harnesses: HarnessInfo[]; canEdit: boolean; onStart: (entry: CatalogEntry) => void; onChanged: () => void }) {
  const { ui, profile } = useApp();
  const send = useCommand();
  const settled = useSettled();
  const [pending, setPending] = useState<"copy" | "keep" | "new" | null>(null);
  const disabled = useDisabled();
  if (!entry) return <Sheet open={false} onClose={onClose} title="">{null}</Sheet>;
  const off = disabled.supported && disabled.isDisabled(entry.disable?.state === "can" ? entry.disable.key ?? entry.host_path : entry.host_path, listedDisabled(entry.disable), entry.disable?.state === "can" && !!entry.disable.key);
  const asks = defaults(entry, harnesses);
  const file = entry.library;
  const builtIn = entry.source === "ophio_setup";
  const noun = entry.kind === "agent" ? "agent" : "skill";
  const openFile = (at: CatalogFile, edit?: boolean) => { onClose(); ui.push({ screen: "file", rootId: at.root_id, path: at.path, edit }); };
  /** Øphio's own agent or skill, copied into the person's folder and opened to edit. */
  const copy = async () => {
    setPending("copy");
    const receipt = await send({ command: "copy_setup_skill", id: entry.id }, { label: `Copy ${entry.name}` });
    const made = await settled(receipt, "setup_skill_copied");
    setPending(null);
    if (!made) return;
    onChanged();
    if (made.file) openFile(made.file, true);
    else { onClose(); toast(`Copied to ${made.host_path} on ${profile.name}. Library can’t reach that folder, so edit it there.`); }
  };
  const choose = async (keepMine: boolean) => {
    setPending(keepMine ? "keep" : "new");
    const receipt = await send(
      { command: "choose_setup_skill_version", id: entry.id, keep_mine: keepMine },
      { label: keepMine ? `Keep your ${entry.name}` : `Use the new ${entry.name}`, success: keepMine ? "Kept yours as it is." : "Using the new version. Yours is in Library’s Recently deleted." },
    );
    setPending(null);
    if (!receipt) return;
    onChanged();
    onClose();
  };
  return (
    <Sheet
      open
      onClose={onClose}
      title={entry.name}
      subtitle={`${entry.kind === "agent" ? "Agent" : "Skill"} · ${SOURCE_LABEL[entry.source]}${entry.copy ? " · your copy" : ""}`}
      // A disabled entry is never started from, here or by any agent.
      footer={off ? undefined : <Button variant="primary" size="lg" block onClick={() => onStart(entry)}>Start from this</Button>}
    >
      {entry.copy?.newer && (
        <Notice
          title={`Øphio has a newer version of this ${noun}`}
          action={
            <>
              <Button size="sm" disabled={!canEdit} pending={pending === "keep"} onClick={() => void choose(true)}>Keep mine</Button>
              <Button size="sm" variant="secondary" disabled={!canEdit} pending={pending === "new"} onClick={() => void choose(false)}>Use the new version</Button>
            </>
          }
        >
          Yours is kept as you edited it until you choose. Taking the new one moves yours to Library’s Recently deleted.
        </Notice>
      )}
      {entry.description && <p className="launch-sheet__desc">{entry.description}</p>}
      <dl className="launch-sheet__facts">
        <div><dt>Starts with</dt><dd>{asks ?? "The agent and model you choose in Review"}{entry.launch_policy ? " · its own permission setting" : ""}</dd></div>
        <div><dt>File</dt><dd className="mono"><PathText path={entry.host_path} /></dd></div>
      </dl>
      <p className="fine">Everything here can still be changed in Review before it starts.</p>
      {file ? (
        <Button variant="secondary" icon={canEdit ? Pencil : FileText} onClick={() => openFile(file)}>
          {canEdit ? "Edit its file" : "Open its file"}
        </Button>
      ) : builtIn && !entry.copy && canEdit ? (
        <>
          <Button variant="secondary" icon={Pencil} pending={pending === "copy"} onClick={() => void copy()}>Customize</Button>
          <p className="fine">Customizing makes your own copy in Øphio’s {noun}s folder on {profile.name} and opens it to edit. Tasks start from your copy from then on, and Øphio’s updates never write over it.</p>
        </>
      ) : null}
      <DisableControl
        disable={entry.disable}
        target={{
          kind: entry.kind,
          host_path: entry.host_path,
          key: entry.disable?.state === "can" ? entry.disable.key : undefined,
          name: entry.name,
          description: entry.description,
          scope: SOURCE_LABEL[entry.source],
          harness: entry.harness,
          library: entry.library,
        }}
      />
    </Sheet>
  );
}

function NewAgentSheet({ open, onClose, onMade }: { open: boolean; onClose: () => void; onMade: () => void }) {
  const { ui, profile } = useApp();
  const send = useCommand();
  const settled = useSettled();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const close = () => { setName(""); setDescription(""); setError(null); onClose(); };
  const make = async () => {
    if (!name.trim()) { setError("Give it a name."); return; }
    setPending(true);
    const receipt = await send(
      { command: "create_agent", name: name.trim(), ...(description.trim() ? { description: description.trim() } : {}) },
      { label: `Make ${name.trim()}` },
    );
    const made = await settled(receipt, "agent_created");
    setPending(false);
    if (!made) return;
    onMade();
    close();
    if (made.file) ui.push({ screen: "file", rootId: made.file.root_id, path: made.file.path });
    else toast(`Made at ${made.host_path} on ${profile.name}. Library can’t reach that folder, so write it there.`);
  };
  return (
    <Sheet
      open={open}
      onClose={close}
      title="New agent"
      subtitle={`On ${profile.name}`}
      footer={<Button variant="primary" size="lg" block pending={pending} onClick={() => void make()}>Make and write it</Button>}
    >
      <TextField label="Name" value={name} onChange={(v) => { setName(v); setError(null); }} placeholder="e.g. Video producer" error={error ?? undefined} />
      <TextArea label="Description" value={description} onChange={setDescription} rows={2} placeholder="One line on what it’s for" hint="Shown on its card here and to Claude Code." />
      <p className="fine">Its file opens next, to write its personality and instructions. It’s a Markdown file Claude Code can use as one of its own agents too.</p>
    </Sheet>
  );
}
