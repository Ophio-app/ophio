// SPDX-License-Identifier: Apache-2.0
//
// What a conversation's agent may reach, under its header, as one quiet line
// ("Read: Home · Change: Nothing" over "Internet on") that opens to its three
// controls: what it may read on the left, the internet in the middle, what it
// may change and run on the right. The computer holds the agent to it
// (`conversation_access`); this only shows and changes it, at any time,
// mid-turn too. Before the first message it is the draft's and goes with the
// conversation; after, each change goes to the computer at once. The control
// turns into a spinner until the computer says the change is in force, which
// can take a while mid-turn: some changes stop the agent's turn and continue it.
//
// Anything past reading its own project is one marker on the line, never
// behind the opener. What the person lets a chat reach is remembered for the
// project's next chat (`set_chat_access_default`), on the computer, from
// their own changes here only; the line's panel says when new chats start
// wider than read-only and sets that back in one tap. Neither shows as in
// force before the computer confirms it.
//
// Whole computer leads either list and warns before it is chosen. What the
// agent was last kept from shows under the line, tinted so it isn't missed,
// with Allow. A host on the person's own network (their LAN or tailnet) needs
// more than the internet, so its Allow turns that on too; the internet
// control shows it, and turning the internet off turns both off. A
// conversation turned into a task keeps its access, so the task keeps the line.

import { Ban, ChevronDown, Eye, ShieldAlert, FolderGit2, FolderPlus, Globe, GlobeOff, House, LoaderCircle, Monitor, Network, Pencil, type LucideIcon } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { describeError, shallowEqual, useApp, useCommand, useHost, useLink, useUi } from "../../app/context";
import { draftKey } from "../../app/ui";
import { useWarning } from "../../app/warnings";
import { Button, IconButton } from "../../design/Button";
import { TextField } from "../../design/Field";
import { PathText } from "../../design/PathText";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import { commandResult } from "../../host/commandResult";
import type { ConversationAccess, Project, Task } from "../../protocol/types";
import type { HostSession } from "../../host/session";
import { matchingProjects } from "../../shell/ProjectSheet";
import { useMay } from "../../shell/NotAllowed";
import { isTaskRunning } from "../../state/projection";
import { accessHeld, newChatAccess, normalizedAccess, reachesOwnNetwork, widerThanRead } from "./access";
import { WORK_CARDS_FEATURE } from "./workCards";
import { NEW_CONVERSATION } from "./conversation";
import { accessBarShown, useMayDoRequest } from "./MayDoLine";
import "./accessBar.css";

type Side = "read" | "write";
/** What can be waiting on the computer: either side, the internet, or Allow. */
type Control = Side | "internet" | "allow" | "default";

/** A change the computer takes longer than this over shows its spinner, so a
 * quick one doesn't flicker. */
const SPIN_AFTER_MS = 250;
/** How long a change may take to be in force: the computer gives a stopped
 * turn 30 s to end before it continues it. */
const APPLY_WAIT_MS = 45_000;
/** How long the bar stays outlined once asked for. */
const CALLED_MS = 1_600;

const widened = (a: ConversationAccess, side: Side): ConversationAccess => (side === "write" ? { ...a, whole_write: true } : { ...a, whole_read: true });

/** Remembers what the person let a chat in this project reach, for its next
 * chat, on a computer that keeps it. Quiet: the chat's own access is already
 * in force, and failing only leaves the next chat where it was. */
export async function rememberChatAccess(session: HostSession, projectId: string, access: ConversationAccess | null): Promise<boolean> {
  const s = session.state.get();
  if (!s.capabilities?.features?.includes(WORK_CARDS_FEATURE)) return false;
  try {
    const receipt = await session.command({ command: "set_chat_access_default", project_id: projectId, access }, { label: access ? "Remember for new chats" : "Start new chats read-only", expectedRevision: null });
    await commandResult(session, receipt, { waitMs: APPLY_WAIT_MS });
    return true;
  } catch {
    return false;
  }
}

/** The line, for a conversation, or a task carried on from one, on a
 * computer that holds it to what is chosen; or, with `newTask`, for an Ask
 * task drafted with what it may reach, such as a suggestion's, in Review,
 * where it reads and changes nothing. */
export function AccessBar({ project, task, newTask = false }: { project: Project; task: Task | undefined; newTask?: boolean }) {
  const { ui, session, profile } = useApp();
  const execute = useCommand();
  const [waiting, track] = useWaiting();
  const pid = project.project_id;
  const held = useHost((s) => accessHeld(s.capabilities));
  const remembers = useHost((s) => !!s.capabilities?.features?.includes(WORK_CARDS_FEATURE));
  const conversing = useUi((s) => s.conversing);
  const draftId = newTask ? null : NEW_CONVERSATION;
  const drafted = useUi((s) => s.drafts[draftKey(pid, draftId)]?.access);
  const names = useHost((s) => Object.fromEntries(Object.values(s.projects).map((p) => [p.project_id, p.name])), shallowEqual);
  const may = useMay("tasks");
  const online = useLink().state === "online";
  const [side, setSide] = useState<Side | null>(null);
  const [wide, setWide] = useState<Side | null>(null);
  const [open, setOpen] = useState(false);
  // Asked for from the line saying what it may do: the controls open, take
  // focus and are outlined for a moment, so the eye finds them.
  const root = useRef<HTMLDivElement>(null);
  const [called, setCalled] = useState(false);
  useMayDoRequest(task?.task_id, !!task && accessBarShown(task, held), () => { setOpen(true); setCalled(true); });
  useEffect(() => {
    if (!called) return;
    const el = root.current;
    el?.scrollIntoView?.({ block: "nearest" });
    const first = el?.querySelector<HTMLButtonElement>(".access__pick:not(:disabled), .access__net:not(:disabled)");
    (first ?? el)?.focus({ preventScroll: true });
    const done = setTimeout(() => setCalled(false), CALLED_MS);
    return () => clearTimeout(done);
  }, [called]);
  const shown = newTask ? !!drafted : task ? !!task.conversation || !!task.access : conversing;
  if (!held || !shown || task?.state === "archived") return null;
  const access = task?.access ?? (newTask ? drafted! : newChatAccess(project, pid, drafted, names));
  // A change to a conversation needs the computer; a draft's stays here.
  const locked = !may || (!!task && !online);
  // Its own chats' choices are remembered for the next chat; a task carried
  // on from one is no longer a chat.
  const ownChat = !newTask && (!task || !!task.conversation);
  const startsWide = remembers && !!project.chat_access && widerThanRead(project.chat_access, pid);

  /** Keeps the change, and resolves once it is in force: at once for a
   * draft, when the computer says so for a conversation. */
  const change = async (next: ConversationAccess, label: string, control: Control): Promise<boolean> => {
    const access = normalizedAccess(next);
    if (!task) {
      // The draft's own choice; it is remembered for the next chat once this
      // one starts with it.
      ui.saveDraft(pid, draftId, { access });
      return true;
    }
    return track(control, async () => {
      const receipt = await execute({ command: "set_conversation_access", task_id: task.task_id, access }, { label });
      if (!receipt) return false;
      try {
        await commandResult(session, receipt, { waitMs: APPLY_WAIT_MS });
      } catch (e) {
        toast(describeError(e, profile.name), { tone: "stop" });
        return false;
      }
      if (ownChat) void rememberChatAccess(session, pid, access);
      return true;
    });
  };
  const internet = async (on: boolean) => {
    if (await change({ ...access, internet: on, local_network: false }, on ? "Turn the internet on" : "Turn the internet off", "internet")) toast(on ? "Internet on" : "Internet off", { tone: "good" });
  };
  /** New chats here start read-only again; a draft does too. */
  const setBack = () => track("default", async () => {
    if (!(await rememberChatAccess(session, pid, null))) {
      toast("Couldn’t reach the computer. New chats still start as before.", { tone: "stop" });
      return false;
    }
    if (!task) ui.saveDraft(pid, NEW_CONVERSATION, { access: undefined });
    toast("New chats here start read-only", { tone: "good" });
    return true;
  });
  const netBusy = waiting.has("internet");
  const local = reachesOwnNetwork(access);
  const busy = waiting.size > 0 && !waiting.has("default");
  const wider = widerThanRead(access, pid);
  const read = summary("read", access, project, names);
  const write = summary("write", access, project, names);
  const net = local ? "Internet and your own network" : access.internet ? "Internet on" : "Internet off";

  return (
    <div ref={root} className={["access", called && "is-called", open && "is-open"].filter(Boolean).join(" ")} role="group" aria-label="What the agent may reach" tabIndex={-1}>
      <button
        type="button"
        className="access__line"
        aria-expanded={open}
        aria-controls={`access-${pid}`}
        aria-label={`What the agent may reach. Read: ${read}. Change: ${write}. ${net}.${wider ? " More than read-only." : ""}`}
        onClick={() => setOpen((o) => !o)}
      >
        <span className="access__line-text">
          <span className="access__line-main">Read: {read} · Change: {write}</span>
          <span className="access__line-sub">
            {busy ? <><LoaderCircle className="access__line-spin" aria-hidden />Changing…</> : net}
            {wider && <span className="access__wider"><ShieldAlert aria-hidden strokeWidth={2.2} />More than read-only</span>}
          </span>
        </span>
        <ChevronDown className="access__line-chevron" aria-hidden strokeWidth={2.1} />
      </button>
      <div id={`access-${pid}`} className="access__panel" hidden={!open}>
        <div className="access__bar">
          <AccessButton side="read" summary={read} busy={waiting.has("read")} disabled={locked} onOpen={() => setSide("read")} />
          <IconButton
            className={["access__net", access.internet && "is-on", local && "is-local", netBusy && "is-busy"].filter(Boolean).join(" ")}
            icon={netBusy ? LoaderCircle : local ? Network : access.internet ? Globe : GlobeOff}
            label={local ? "Internet and your own network" : "Internet"}
            pressed={access.internet}
            aria-busy={netBusy || undefined}
            disabled={locked || netBusy}
            onClick={() => void internet(!access.internet)}
          />
          {/* An Ask task only reads. */}
          {!newTask && <AccessButton side="write" summary={write} busy={waiting.has("write")} disabled={locked} onOpen={() => setSide("write")} />}
        </div>
        {startsWide && ownChat && (
          <div className="access__default">
            <p className="access__default-text">New chats in {project.name} start with what you last let a chat here reach.</p>
            <Button size="sm" pending={waiting.has("default")} disabled={!may || !online} onClick={() => void setBack()}>Set back to read-only</Button>
          </div>
        )}
      </div>
      {task?.access_blocked && <Blocked key={task.access_blocked.at} task={task} access={access} busy={waiting.has("allow")} disabled={locked} onAllow={(next, label) => change(next, label, "allow")} onWhole={setWide} />}
      <AccessSheet
        side={side}
        onClose={() => setSide(null)}
        project={project}
        what={newTask || (task && !task.conversation) ? "task" : "conversation"}
        access={access}
        note={task && <Running task={task} />}
        onApply={(next, s) => void change(next, s === "write" ? "Change what the agent may change" : "Change what the agent may read", s)}
      />
      <WholeComputer side={wide} onClose={() => setWide(null)} onAgree={(s) => { setWide(null); void change(widened(access, s), "Let the agent reach the whole computer", s); }} />
    </div>
  );
}

/** The controls whose change the computer hasn't yet said is in force, once
 * it has taken long enough to show; and what runs a change for a control. */
function useWaiting(): [ReadonlySet<Control>, (control: Control, work: () => Promise<boolean>) => Promise<boolean>] {
  const [waiting, setWaiting] = useState<ReadonlySet<Control>>(new Set());
  const track = async (control: Control, work: () => Promise<boolean>) => {
    const timer = setTimeout(() => setWaiting((w) => new Set(w).add(control)), SPIN_AFTER_MS);
    try {
      return await work();
    } finally {
      clearTimeout(timer);
      setWaiting((w) => {
        const next = new Set(w);
        next.delete(control);
        return next;
      });
    }
  };
  return [waiting, track];
}

/** While the agent works, that a change reaches it at once. */
function Running({ task }: { task: Task }) {
  const running = useHost((s) => isTaskRunning(s, task));
  return running ? <p className="access-sheet__note">The agent is working. Changes reach it right away; taking something away may restart what it’s doing.</p> : null;
}

/** "Read" or "Change & run" over what it covers, as one button that opens
 * its list; the spinner stands in for its icon while a change is applied. */
function AccessButton({ side, summary, busy, disabled, onOpen }: { side: Side; summary: string; busy: boolean; disabled: boolean; onOpen: () => void }) {
  const title = side === "read" ? "Read" : "Change & run";
  const Icon = busy ? LoaderCircle : side === "read" ? Eye : Pencil;
  return (
    <button
      type="button"
      className={["access__pick", `access__pick--${side}`, busy && "is-busy"].filter(Boolean).join(" ")}
      disabled={disabled || busy}
      onClick={onOpen}
      aria-haspopup="dialog"
      aria-busy={busy || undefined}
      aria-label={`${title}: ${summary}`}
    >
      <span className="access__label"><Icon aria-hidden strokeWidth={2.1} />{title}</span>
      <span className="access__value"><span className="access__summary">{summary}</span><ChevronDown className="access__chevron" aria-hidden strokeWidth={2.1} /></span>
    </button>
  );
}

/** What one side covers, in a few words for its button. */
export function summary(side: Side, access: ConversationAccess, here: Project, names: Record<string, string>): string {
  if (side === "read" ? access.whole_read || access.whole_write : access.whole_write) return "Whole computer";
  const ids = side === "read" ? [here.project_id, ...access.read, ...access.write] : access.write;
  const shown = [...new Set(ids)].map((id) => (id === here.project_id ? here.name : names[id])).filter((n): n is string => !!n);
  if (shown.length === 0) return "Nothing";
  return shown.length === 1 ? shown[0]! : `${shown[0]} +${shown.length - 1}`;
}

/** What the agent was last kept from, and Allow, which lets it. */
function Blocked({ task, access, busy, disabled, onAllow, onWhole }: {
  task: Task;
  access: ConversationAccess;
  busy: boolean;
  disabled: boolean;
  onAllow: (next: ConversationAccess, label: string) => Promise<boolean>;
  onWhole: (side: Side) => void;
}) {
  const b = task.access_blocked!;
  const allow = async () => {
    if (b.host && b.local) {
      if (await onAllow({ ...access, internet: true, local_network: true }, "Let the agent reach your own network")) toast("Your own network on", { tone: "good" });
    } else if (b.host) {
      if (await onAllow({ ...access, internet: true }, "Turn the internet on")) toast("Internet on", { tone: "good" });
    } else if (b.project_id) {
      const id = b.project_id;
      await onAllow(b.write ? { ...access, write: [...access.write, id] } : { ...access, read: [...access.read, id] }, b.write ? "Let the agent change it" : "Let the agent read it");
    } else {
      // Outside every project, only the whole computer reaches it.
      onWhole(b.write ? "write" : "read");
    }
  };
  return (
    <div className="access-blocked" role="status">
      <Ban className="access-blocked__icon" aria-hidden />
      <p className="access-blocked__text">
        <strong className="access-blocked__lead">Blocked:</strong> tried to{" "}
        {b.host ? (
          <>reach <span className="access-blocked__what">{b.host}</span>{b.local && " on your own network"}</>
        ) : (
          <>{b.write ? "write to" : "read"} <span className="access-blocked__what">{b.path ? <PathText path={b.path} /> : "a file"}</span></>
        )}
      </p>
      <Button size="sm" pending={busy} disabled={disabled} onClick={() => void allow()}>Allow</Button>
    </div>
  );
}

/**
 * One side's list: Whole computer first, then Home and the projects, which a
 * search narrows. The field isn't focused on opening, so the keyboard comes up
 * only when it's tapped. Choices apply on Done; closing keeps what was there.
 */
function AccessSheet({ side, onClose, project, what, access, note, onApply }: {
  side: Side | null;
  onClose: () => void;
  project: Project;
  what: "conversation" | "task";
  access: ConversationAccess;
  note: ReactNode;
  onApply: (next: ConversationAccess, side: Side) => void;
}) {
  const [staged, setStaged] = useState(access);
  const [opened, setOpened] = useState(access);
  const [query, setQuery] = useState("");
  const [wide, setWide] = useState<Side | null>(null);
  const projects = useHost((s) => Object.values(s.projects), shallowEqual);
  useEffect(() => {
    if (side) { setStaged(access); setOpened(access); setQuery(""); }
    // Only on opening: what the computer says meanwhile doesn't undo a choice.
  }, [side]);
  const read = side !== "write";
  const places = matchingProjects([...projects].sort((a, b) => Number(!!b.home) - Number(!!a.home) || a.name.localeCompare(b.name)), query);
  const whole = read ? staged.whole_read || staged.whole_write : staged.whole_write;
  const changed = JSON.stringify(normalizedAccess(staged)) !== JSON.stringify(normalizedAccess(access));
  // Unapplied ticks outlast another screen in front, unless what the agent
  // may reach changed meanwhile: Done would then put back what was there.
  const current = JSON.stringify(normalizedAccess(opened)) === JSON.stringify(normalizedAccess(access));
  const key = read ? "read" : "write";

  const toggleWhole = (on: boolean) => {
    if (on) setWide(side);
    else setStaged((s) => (read ? { ...s, whole_read: false } : { ...s, whole_write: false }));
  };
  const toggle = (id: string, on: boolean) => setStaged((s) => ({ ...s, [key]: on ? [...s[key], id] : s[key].filter((x) => x !== id) }));

  return (
    <>
      <Sheet
        open={side !== null}
        keepWhenHidden={current}
        onClose={onClose}
        size="tall"
        className="access-sheet"
        title={read ? "What the agent may read" : "What the agent may change and run"}
        subtitle={`In this ${what} in ${project.name}`}
        footer={<Button variant="primary" size="lg" block onClick={() => { if (changed && side) onApply(staged, side); onClose(); }}>Done</Button>}
      >
        {note}
        <TextField label="Search projects" hideLabel placeholder="Search projects" type="search" value={query} onChange={setQuery} enterKeyHint="search" autoComplete="off" />
        <div className="access-sheet__list" role="group" aria-label={read ? "Read" : "Change and run"}>
          {!query.trim() && (
            <Option
              icon={Monitor}
              title="Whole computer"
              checked={whole}
              disabled={read && staged.whole_write}
              subtitle={read && staged.whole_write ? "It can change the whole computer, so it can read it" : read ? "Every file you can read" : "Every file you can change, and any command"}
              onChange={toggleWhole}
            />
          )}
          {places.map((p) => {
            const own = p.project_id === project.project_id;
            const writes = staged.write.includes(p.project_id);
            const implied = whole || (read && (own || writes));
            return (
              <Option
                key={p.project_id}
                icon={p.home ? House : p.one_off ? FolderPlus : FolderGit2}
                title={p.name}
                checked={implied || staged[key].includes(p.project_id)}
                disabled={implied}
                subtitle={whole ? "Part of the whole computer"
                  : read && own ? `This ${what}’s project, always readable`
                  : read && writes ? "It can change this, so it can read it"
                  : <PathText path={p.host_path} />}
                onChange={(on) => toggle(p.project_id, on)}
              />
            );
          })}
          {places.length === 0 && <p className="access-sheet__none">No project matches “{query.trim()}”.</p>}
        </div>
      </Sheet>
      <WholeComputer side={wide} onClose={() => setWide(null)} onAgree={(s) => { setWide(null); setStaged((x) => widened(x, s)); }} />
    </>
  );
}

function Option({ icon: Icon, title, subtitle, checked, disabled, onChange }: {
  icon: LucideIcon;
  title: string;
  subtitle: ReactNode;
  checked: boolean;
  disabled?: boolean;
  onChange: (on: boolean) => void;
}) {
  return (
    <label className={["row", "access-option", checked && "is-selected", disabled && "is-implied"].filter(Boolean).join(" ")}>
      <span className="row__icon" aria-hidden><Icon strokeWidth={2} /></span>
      <span className="row__main">
        <span className="row__text">
          <span className="row__title">{title}</span>
          <span className="row__subtitle">{subtitle}</span>
        </span>
      </span>
      <input type="checkbox" className="access-option__check" checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
    </label>
  );
}

/** The warning before the whole computer, which can be turned off. */
function WholeComputer({ side, onClose, onAgree }: { side: Side | null; onClose: () => void; onAgree: (side: Side) => void }) {
  const warn = useWarning("access.whole_computer");
  const [asking, setAsking] = useState<Side | null>(null);
  // Asked for, it opens the warning, or goes ahead when that's turned off.
  useEffect(() => {
    if (!side) { setAsking(null); return; }
    warn.ask(() => setAsking(side), () => onAgree(side));
  }, [side]);
  const write = asking === "write";
  return (
    <Sheet
      open={asking !== null}
      onClose={onClose}
      title={write ? "Let it change your whole computer?" : "Let it read your whole computer?"}
      footer={<Button variant="warn" size="lg" block onClick={() => { if (!asking) return; warn.agreed(); onAgree(asking); }}>{write ? "Let it change everything" : "Let it read everything"}</Button>}
    >
      <p>{write
        ? "The agent will be able to change or delete any file you can, and run any command, anywhere on this computer."
        : "The agent will be able to read any file you can, in every project and outside them, such as documents and keys."}</p>
      <p>You can take this back at any time, even while it works.</p>
      {warn.check}
    </Sheet>
  );
}
