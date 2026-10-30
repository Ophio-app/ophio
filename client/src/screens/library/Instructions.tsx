// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import type { InstructionSource } from "../../protocol/types";
import { PROGRAM_NAME } from "../../brand";
import { Screen } from "../../shell/Screen";
import { Button } from "../../design/Button";
import { Empty } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Age } from "../../design/Age";
import { ConnectionNote } from "./shared";
import { bytes } from "./model";
import type { RouteProps } from "../types";
import "./library.css";
import { InlineCode } from "../../design/InlineCode";

export function instructionStatus(source: InstructionSource): string {
  if (source.state === "unknown")
    return "The computer couldn’t inspect this source";
  if (source.state === "missing") return "Not found";
  if (source.scope === "settings")
    return source.state === "not_applied"
      ? "Work runs ignore these settings"
      : "Work runs use these settings";
  if (source.state === "not_applied")
    return "Runs of this project don’t read this source";
  return source.loaded_reported || source.state === "loaded"
    ? "Agent reported loading this source"
    : "Discovered, not confirmed loaded";
}
// Files the agent reported loading come first, then files the computer read,
// then ones it found but couldn't inspect. Places with no file are collapsed.
function rank(source: InstructionSource): number {
  if (source.loaded_reported || source.state === "loaded") return 0;
  return source.state === "unknown" ? 2 : 1;
}
function groupSources(sources: InstructionSource[]): {
  found: InstructionSource[];
  missing: InstructionSource[];
} {
  return {
    found: sources
      .filter((s) => s.state !== "missing")
      .sort((a, b) => rank(a) - rank(b)),
    missing: sources.filter((s) => s.state === "missing"),
  };
}
function checkedSummary({
  found,
  missing,
}: ReturnType<typeof groupSources>): string {
  const places = missing.length === 1 ? "place" : "places";
  return `${missing.length}${found.length > 0 ? " other" : ""} ${places} checked; none found`;
}
export default function Instructions({ route }: RouteProps<"instructions">) {
  const { session, profile, ui } = useApp(),
    online = useLink().state === "online";
  const task = useHost((s) => s.tasks[route.taskId]);
  const [sources, setSources] = useState<InstructionSource[] | null>(null),
    [error, setError] = useState<string | null>(null),
    [retry, setRetry] = useState(0),
    [at, setAt] = useState<number>();
  useEffect(() => {
    if (!online || !task) return;
    let alive = true;
    void session
      .query({
        kind: "instructions",
        project_id: task.project_id,
        task_id: task.task_id,
      })
      .then((r) => {
        if (alive) {
          setSources(r.sources);
          setAt(Date.now());
          setError(null);
        }
      })
      .catch((e: unknown) => {
        if (alive) setError(describeError(e, profile.name));
      });
    return () => {
      alive = false;
    };
  }, [
    session,
    online,
    task?.task_id,
    task?.project_id,
    task?.current_run,
    profile.name,
    retry,
  ]);
  const groups = sources && groupSources(sources);
  return (
    <Screen
      title="Instructions"
      subtitle={task?.title}
      className="library-screen"
    >
      <div className="lib-stack">
        <ConnectionNote at={at} />
        <Notice>
          Discovery shows what the computer found. It does not prove the agent
          loaded it or reloaded later edits.
        </Notice>
        {at && (
          <p className="lib-meta">
            Checked <Age at={at} suffix=" ago" />
          </p>
        )}
        {error && <Notice tone="stop">{error}</Notice>}
        <Button disabled={!online} onClick={() => setRetry((n) => n + 1)}>
          Refresh instructions
        </Button>
        {!task && (
          <Empty title="Task unavailable">Reconnect to load the task.</Empty>
        )}
        {task && sources === null && !error && (
          <p>
            {online
              ? "Discovering instruction files…"
              : "Connect to discover instruction files."}
          </p>
        )}
        {sources?.length === 0 && (
          <Empty title="No instruction sources reported">
            The computer has no discovery evidence for this task.
          </Empty>
        )}
        {groups?.found.map((s, i) => (
          <article className="lib-versions" key={`${s.host_path}:${i}`}>
            <h2>{s.label}</h2>
            {s.label !== (s.path ?? s.host_path) && (
              <p className="lib-path mono">
                {s.path ?? s.host_path ?? "Path unknown"}
              </p>
            )}
            <p className="lib-meta">
              {s.size == null ? s.scope : `${s.scope} · ${bytes(s.size)}`}
            </p>
            <p>{instructionStatus(s)}</p>
            {s.scope === "settings" && s.state === "not_applied" && (
              <p className="lib-meta">
                They can allow tools without asking and run commands. To use
                them, run{" "}
                <InlineCode>
                  {PROGRAM_NAME} projects settings {task?.project_id} on
                </InlineCode>{" "}
                on the computer.
              </p>
            )}
            {s.state === "changed" && (
              <Notice tone="warn">
                Changed since this run started.
                {s.revision_at_start !== null &&
                  (s.current_revision === null
                    ? " The file is missing or unreadable now."
                    : " The current file differs from the run’s starting version.")}
              </Notice>
            )}
            {/* Outside Library folders the computer keeps the versions to itself. */}
            {(s.revision_at_start !== null || s.current_revision !== null) && (
              <p className="lib-meta">
                At run start:{" "}
                <InlineCode>{s.revision_at_start?.slice(0, 12) ?? "Unknown"}</InlineCode>
                <br />
                Current:{" "}
                <InlineCode>{s.current_revision?.slice(0, 12) ?? "Unknown"}</InlineCode>
              </p>
            )}
            {s.root_id && s.path && (
              <Button
                onClick={() => {
                  if (s.root_id && s.path)
                    ui.push({
                      screen: "file",
                      rootId: s.root_id,
                      path: s.path,
                      taskId: route.taskId,
                      source: "instructions",
                    });
                }}
              >
                Open in Library
              </Button>
            )}
          </article>
        ))}
        {groups && groups.missing.length > 0 && (
          <details className="lib-options">
            <summary>{checkedSummary(groups)}</summary>
            <ul role="list" className="lib-checked">
              {groups.missing.map((s, i) => (
                <li
                  key={`${s.host_path}:${i}`}
                  className="lib-path lib-meta mono"
                >
                  {s.host_path ?? s.path ?? "Path unknown"}
                </li>
              ))}
            </ul>
          </details>
        )}
      </div>
    </Screen>
  );
}
