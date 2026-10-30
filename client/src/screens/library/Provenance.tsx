// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import type { Route } from "../../app/ui";
import type { RouteProps } from "../types";
import type { InstructionSource } from "../../protocol/types";
import { Notice } from "../../design/Notice";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { useRoute } from "../../shell/Screen";
import { InlineCode } from "../../design/InlineCode";

/** Receipt links already persist their task in the immediately preceding route. */
export function readerContext(
  route: RouteProps<"file">["route"],
  parent: Route | undefined,
): RouteProps<"file">["route"] {
  return !route.taskId && parent?.screen === "receipt"
    ? { ...route, taskId: parent.taskId, source: "receipt" }
    : route;
}

export function provenanceTitle(source: InstructionSource | null): string {
  if (!source || source.state === "unknown")
    return "Instruction status unknown";
  if (source.state === "missing") return "Instruction file not found";
  if (source.state === "changed")
    return "Changed since this task’s run started";
  return "Instruction file discovered";
}
export function FileProvenance({
  route,
  revision,
}: RouteProps<"file"> & { revision?: string }) {
  const { session, profile } = useApp(),
    online = useLink().state === "online",
    { active } = useRoute();
  const task = useHost((s) =>
      route.taskId ? s.tasks[route.taskId] : undefined,
    ),
    grant = useHost((s) => s.grants?.library_read);
  const [evidence, setEvidence] = useState<{
      source: InstructionSource | null;
      at: number;
      task: string;
      run: string | null;
      rootId: string;
      path: string;
    } | null>(null),
    [error, setError] = useState<string | null>(null),
    [retry, setRetry] = useState(0);
  useEffect(() => {
    if (!route.taskId || !task || !online || !grant || !active) return;
    let alive = true;
    void session
      .query({
        kind: "instructions",
        project_id: task.project_id,
        task_id: task.task_id,
      })
      .then((r) => {
        if (alive) {
          setEvidence({
            source:
              r.sources.find(
                (s) =>
                  s.root_id === route.rootId &&
                  s.path === route.path &&
                  s.harness === task.harness,
              ) ?? null,
            at: Date.now(),
            task: task.task_id,
            run: task.current_run,
            rootId: route.rootId,
            path: route.path,
          });
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
    profile.name,
    route.taskId,
    route.rootId,
    route.path,
    task?.project_id,
    task?.harness,
    task?.current_run,
    online,
    grant,
    active,
    retry,
  ]);
  if (!route.taskId) return null;
  const current =
    evidence?.rootId === route.rootId &&
    evidence?.path === route.path &&
    evidence?.task === task?.task_id &&
    evidence?.run === task?.current_run
      ? evidence
      : null;
  const source = current?.source ?? null;
  return (
    <Notice
      tone={source?.state === "changed" ? "warn" : "unknown"}
      title={provenanceTitle(source)}
    >
      <div className="lib-stack">
        <p>
          For {task?.title ?? "the task this file was opened from"}.{" "}
          {source?.loaded_reported || source?.state === "loaded"
            ? "The agent reported this source; use of the current revision is not confirmed."
            : "Not confirmed by the agent. Discovery does not prove it read this file."}
        </p>
        {source?.state === "changed" && (
          <p>
            The current file differs from the starting observation. Saving does
            not prove the agent reloaded it.
          </p>
        )}
        {current && (
          <p className="lib-meta">
            Observed <Age at={current.at} suffix=" ago" /> · Starting revision{" "}
            <InlineCode>{source?.revision_at_start?.slice(0, 12) ?? "unknown"}</InlineCode>
          </p>
        )}
        {source?.current_revision &&
          revision &&
          source.current_revision !== revision && (
            <p>
              The reader shows a different revision from this instruction
              observation. Reopen the file to read its current text.
            </p>
          )}
        {error && <p className="error-text">{error}</p>}
        <Button
          disabled={!online || !task || !grant}
          onClick={() => setRetry((n) => n + 1)}
        >
          Refresh instruction status
        </Button>
      </div>
    </Notice>
  );
}
