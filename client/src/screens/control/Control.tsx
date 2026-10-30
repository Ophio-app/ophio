// SPDX-License-Identifier: Apache-2.0
import { terminalTitle } from "./terminalTitle";
import { useTerminalEnds } from "./terminalHistory";
import { clockTime } from "../../design/time";
import { toast } from "../../design/Toast";
import { commandResult } from "../../host/commandResult";
import { useEffect, useState } from "react";
import { Plus, TerminalSquare } from "lucide-react";
import {
  useApp,
  useCommand,
  useHost,
  useLink,
  useUi,
  shallowEqual,
  describeError,
} from "../../app/context";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { CapabilityNote, availabilityText } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { isActiveTask } from "../../state/projection";
import type { HostCapabilities, Grants } from "../../protocol/types";
import type { RouteProps } from "../types";
import { useVisible } from "./useControl";
import { privateStepTask } from "./PrivateStepWarning";
import "./control.css";
import { ControlRoot } from "./ControlRoot";
import { PathText } from "../../design/PathText";
import { NotAllowed } from "../../shell/NotAllowed";
import { useWarning } from "../../app/warnings";
import { OtherProject, OtherProjects, useByProject, useProjectScope } from "./projectScope";

export default function Control({ route: _route }: RouteProps<"control">) {
  return <ControlRoot />;
}

/** Terminals and previews, asked for again each time Control is shown. */
export function useControlQueries() {
  const { session, profile } = useApp();
  const active = useVisible();
  const online = useLink().state === "online";
  const loaded = useHost((s) => s.host !== null);
  const grants = useHost((s) => s.grants);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  // Unknown grants still query; the computer is the judge.
  const mayTerminal = grants?.terminal !== false,
    mayTasks = grants?.tasks !== false;
  useEffect(() => {
    if (!active || !online) return;
    let disposed = false;
    setLoading(true);
    setError(null);
    const head = session.state.get().journalHead;
    const observedTerminals = session.state.get().terminals;
    const terminalsQuery = !mayTerminal
      ? null
      : session.query({ kind: "terminals" }).then((r) => {
          if (!disposed)
            session.state.set((s) => ({
              ...s,
              terminals:
                s.terminals === observedTerminals
                  ? r.terminals
                  : s.terminals,
            }));
        });
    const projects = mayTasks ? Object.keys(session.state.get().projects) : [];
    const previewQueries = projects.map(async (id) => {
      const r = await session.query({ kind: "previews", project_id: id });
      if (!disposed)
        session.state.set((s) => ({
          ...s,
          previews: {
            ...s.previews,
            [id]:
              s.journalHead === head || !s.previews[id]
                ? r.previews
                : s.previews[id],
          },
        }));
    });
    void Promise.allSettled([terminalsQuery, ...previewQueries]).then(
      (results) => {
        if (disposed) return;
        setLoading(false);
        const failure = results.find((r) => r.status === "rejected");
        if (failure?.status === "rejected")
          setError(describeError(failure.reason, profile.name));
      },
    );
    return () => {
      disposed = true;
    };
  }, [
    active,
    online,
    session,
    profile.name,
    retry,
    loaded,
    mayTerminal,
    mayTasks,
  ]);
  return { loading, error, setError, retry: () => setRetry((n) => n + 1) };
}
export type ControlQueries = ReturnType<typeof useControlQueries>;

/** Computer-use tasks Control lists: running ones, and one paused at a
 * private step on its display. */
export function useComputerUseTasks() {
  return useHost(
    (s) =>
      Object.values(s.tasks).filter(
        (t) => t.kind === "computer_use" && (isActiveTask(t) || (t.target_id && privateStepTask(s, t.target_id)?.task_id === t.task_id)),
      ),
    shallowEqual,
  );
}

export function useAllPreviews() {
  return useHost(
    (s) => Object.values(s.previews).flat(),
    shallowEqual,
  );
}

/** The chosen project's terminal sessions, with New, and those of other
 * projects on request. `compact` lists each as one row that opens it. */
export function Terminals({ queries, compact = false, title = "Terminals" }: {
  queries: ControlQueries;
  compact?: boolean;
  title?: string | null;
}) {
  const { session, ui, profile } = useApp();
  const active = useVisible();
  const online = useLink().state === "online";
  const projectId = useUi((s) => s.projectId);
  const project = useHost((s) =>
    projectId ? s.projects[projectId] : undefined,
  );
  const reportedTerminals = useHost((s) => s.terminals);
  const terminalEnds = useTerminalEnds();
  const removedKey = `terminal.removed.${profile.profileId}`;
  const [removed, setRemoved] = useState<string[]>(() => {
    try {
      const saved: unknown = JSON.parse(localStorage.getItem(removedKey) ?? "[]");
      return Array.isArray(saved) ? saved.filter((id): id is string => typeof id === "string") : [];
    } catch { return []; }
  });
  const terminals = reportedTerminals.filter(t => t.state.state !== "exited" || !removed.includes(t.terminal_id));
  const scope = useProjectScope();
  const shown = terminals.filter((t) => scope.here(t.project_id));
  const elsewhere = useByProject(terminals.filter((t) => !scope.here(t.project_id)), (t) => t.project_id);
  const [others, setOthers] = useState(false);
  const removeEnded = (terminalId: string) => {
    if (!reportedTerminals.some(t => t.terminal_id === terminalId && t.state.state === "exited")) return;
    const next = [...removed.filter(id => id !== terminalId), terminalId];
    try {
      localStorage.setItem(removedKey, JSON.stringify(next));
      setRemoved(next);
    } catch { toast("The ended session could not be removed from this device’s list.", { tone: "stop" }); }
  };
  const caps = useHost((s) => s.capabilities);
  const grants = useHost((s) => s.grants);
  const [creating, setCreating] = useState(false);
  const [newTerminal, setNewTerminal] = useState(false);
  const warn = useWarning("terminal.open");
  const run = useCommand();
  const mayTerminal = grants?.terminal !== false;
  const { loading } = queries;

  const create = async () => {
    setCreating(true);
    const result = await run(
      {
        command: "open_terminal",
        project_id: projectId,
        harness: null,
        cols: 80,
        rows: 24,
      },
      { label: "Open new terminal" },
    );
    try {
      const effect = result ? await commandResult(session, result) : null;
      if (effect?.kind === "terminal_opened") {
        setNewTerminal(false);
        ui.push({
          screen: "terminal",
          terminalId: effect.terminal.terminal_id,
        });
      }
    } catch (error) {
      queries.setError(describeError(error, profile.name));
    } finally {
      setCreating(false);
    }
  };
  const status = (t: (typeof terminals)[number]) =>
    t.state.state === "exited" ? terminalEnds[t.terminal_id]?.closedAt !== undefined
      ? `Closed by you · ${clockTime(terminalEnds[t.terminal_id]!.closedAt!)}`
      : `Exited${t.state.code === null ? " · exit code unknown" : ` with code ${t.state.code}`} · ${terminalEnds[t.terminal_id] ? `observed ${clockTime(terminalEnds[t.terminal_id]!.observedAt)}` : "time not reported"}`
      : t.state.state;
  const compactRow = (t: (typeof terminals)[number]) => {
    const title = terminalTitle(t, reportedTerminals);
    const ended = t.state.state === "exited";
    const open = () => ui.push({ screen: "terminal", terminalId: t.terminal_id });
    const subtitle = <>
      <span className="mono">{homePath(t.cwd)}</span>
      {" · "}{ended ? status(t) : t.last_output_at != null ? <>Output <Age at={t.last_output_at} suffix=" ago" /></> : status(t)}
    </>;
    // A running session is one tap; an ended one keeps View output and
    // Remove at its end, their names saying which session.
    const which = <>{" "}<span className="visually-hidden">{title}</span></>;
    return ended ? (
      <ListRow
        key={t.terminal_id}
        icon={TerminalSquare}
        title={title}
        subtitle={subtitle}
        trailing={<div className="control-keys">
          <Button size="sm" onClick={open}>View output{which}</Button>
          <Button size="sm" title="Remove ended session from this device’s list" onClick={() => removeEnded(t.terminal_id)}>Remove{which}</Button>
        </div>}
      />
    ) : (
      <ListRow
        key={t.terminal_id}
        icon={TerminalSquare}
        title={title}
        ariaLabel={`Attach ${title}`}
        subtitle={subtitle}
        chevron
        onClick={open}
      />
    );
  };
  const listRow = (t: (typeof terminals)[number]) => {
    const title = terminalTitle(t, reportedTerminals);
    // Each session has its own buttons, so their names say which session.
    const which = <>{" "}<span className="visually-hidden">{title}</span></>;
    return (
      <ListRow
        key={t.terminal_id}
        icon={TerminalSquare}
        title={title}
        subtitle={
          <>
            <span className="mono">{t.cwd}</span>
            <span className="control-subline">
              {status(t)}
              {t.last_output_at != null && <> · Output <Age at={t.last_output_at} suffix=" ago" /></>}
            </span>
          </>
        }
        trailing={<div className="control-keys">
          <Button size="sm" onClick={() => ui.push({ screen: "terminal", terminalId: t.terminal_id })}>
            {t.state.state === "exited" ? "View output" : "Attach"}{which}
          </Button>
          {t.state.state === "exited" && <Button size="sm" title="Remove ended session from this device’s list" onClick={() => removeEnded(t.terminal_id)}>Remove{which}</Button>}
        </div>}
      />
    );
  };
  return (
    <Section
      title={title ?? undefined}
      id={title ? "control-terminals" : undefined}
      className={compact ? "control-part" : undefined}
      action={
        <Button
          size="sm"
          icon={Plus}
          disabled={
            !online ||
            !grants?.terminal ||
            caps?.terminal.state !== "available"
          }
          pending={creating && !newTerminal}
          onClick={() => warn.ask(() => setNewTerminal(true), () => void create())}
        >
          New terminal
        </Button>
      }
    >
      {caps && (
        <CapabilityNote what="Terminal" availability={caps.terminal} />
      )}
      {!mayTerminal ? (
        <NotAllowed what="open terminals" permission="terminal" />
      ) : shown.length === 0 ? (
        compact ? <p className="fine control-none">{loading && online ? "Loading terminals…"
          : scope.name ? `No terminal sessions in ${scope.name}. New terminal starts a shell in its folder.`
          : "No terminal sessions. New terminal starts a shell on the computer."}</p> :
        <Empty
          icon={TerminalSquare}
          title={
            loading && online ? "Loading terminals…" : "No terminal sessions"
          }
        >
          New terminal starts a shell on the computer. Attach opens an
          existing session.
        </Empty>
      ) : compact ? (
        <div className="group">{shown.map(compactRow)}</div>
      ) : (
        <div className="control-list">{shown.map(listRow)}</div>
      )}
      {mayTerminal && (
        <OtherProjects count={elsewhere.reduce((n, g) => n + g.items.length, 0)} open={others} onToggle={() => setOthers((o) => !o)}>
          {elsewhere.map((g) => (
            <OtherProject key={g.id ?? ""} name={g.name}>
              {compact ? <div className="group">{g.items.map(compactRow)}</div> : <div className="control-list">{g.items.map(listRow)}</div>}
            </OtherProject>
          ))}
        </OtherProjects>
      )}
      <Sheet
        open={newTerminal && active}
        onClose={() => setNewTerminal(false)}
        title="Open a new terminal"
        footer={
          <Button
            variant="primary"
            block
            icon={Plus}
            pending={creating}
            disabled={!online}
            onClick={() => { warn.agreed(); void create(); }}
          >
            {creating ? "Opening terminal…" : "Open new terminal"}
          </Button>
        }
      >
        <p>A new shell will start on {profile.name}.</p>
        <p className="mono control-path">
          {project ? (
            <PathText path={project.host_path} />
          ) : (
            "The computer’s default directory"
          )}
        </p>
        <p className="fine">
          Closing this view detaches. The shell and its running programs keep
          running until the session is closed.
        </p>
        {warn.check}
      </Sheet>
    </Section>
  );
}

/** A path under a home folder, written from `~`. */
function homePath(path: string): string {
  return path.replace(/^\/(?:home|Users)\/[^/]+(?=\/|$)/, "~");
}

export function computerUseBlocker(online: boolean, projectId: string | null,
  caps: HostCapabilities | null, grants: Grants | null): string | null {
  const blocked = startBlocker(online, projectId, grants);
  if (blocked) return blocked;
  if (!caps) return "The computer hasn’t reported whether computer use is available.";
  const ability = availabilityText(caps.computer_use);
  return ability.ok ? null : `Managed computer use: ${ability.label.toLowerCase()}. ${ability.detail ?? ""}`;
}

/** What on this device keeps computer use from starting, whatever the
 * computer says of it. */
export function startBlocker(online: boolean, projectId: string | null, grants: Grants | null): string | null {
  if (!online) return "Reconnect to the computer before starting computer use.";
  if (!projectId) return "Choose a project before starting computer use.";
  if (!grants) return "Waiting for this device’s permissions.";
  const missing = [
    !grants.tasks && "task",
    !grants.computer_use && "computer use",
    !grants.view && "desktop viewing",
    !grants.control_input && "input",
  ].filter((permission): permission is string => !!permission);
  if (missing.length)
    return `This device needs ${new Intl.ListFormat("en", { type: "conjunction" }).format(missing)} ${missing.length === 1 ? "permission" : "permissions"}.`;
  return null;
}
