// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../../brand";
import { useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { CommandError } from "../../host/session";
import { commandResult } from "../../host/commandResult";
import type {
  HarnessKind,
  ReusableFormat,
  ReusableFormatKind,
} from "../../protocol/types";
import { Screen } from "../../shell/Screen";
import { Button } from "../../design/Button";
import { Empty } from "../../design/ListRow";
import { TextArea, TextField } from "../../design/Field";
import { CapabilityNote, Notice } from "../../design/Notice";
import { StateLine } from "../../design/StateLine";
import { useStore } from "../../state/store";
import { announce } from "../../design/Announcer";
import { ConnectionNote, StorageNote, useLibrary, useRoots } from "./shared";
import { validName } from "./model";
import type { RouteProps } from "../types";
import "./library.css";

const names: Record<ReusableFormatKind, string> = {
  task: "Task",
  skill: "Skill",
  subagent: "Subagent",
  prompt: "Prompt",
};
function asHarness(s: string | undefined, fallback: HarnessKind): HarnessKind {
  return s === "claude_code" ||
    s === "codex" ||
    s === "hermes" ||
    s === "open_claw" ||
    s === "pi"
    ? s
    : fallback;
}
function asKind(s: string | undefined): ReusableFormatKind {
  return s === "skill" || s === "subagent" || s === "prompt" ? s : "task";
}
export default function Reusable({ route }: RouteProps<"reusable">) {
  const { session, profile, ui, platform } = useApp(),
    online = useLink().state === "online",
    store = useLibrary(),
    roots = useRoots();
  const task = useHost((s) => s.tasks[route.taskId]),
    grants = useHost((s) => s.grants),
    harnesses = useHost((s) => s.harnesses);
  const key = `reusable:${route.taskId}`,
    forms = useStore(store, (s) => s.forms);
  const harness = asHarness(
      forms[`${key}:harness`],
      task?.harness ?? "claude_code",
    ),
    kind = asKind(forms[`${key}:kind`]);
  const previewKey = `${key}:${harness}:${kind}`;
  const [catalog, setCatalog] = useState<{
      harness: HarnessKind;
      taskId: string;
      version: string | null;
      formats: ReusableFormat[];
    } | null>(null),
    [busy, setBusy] = useState(false),
    [error, setError] = useState<string | null>(null),
    [retry, setRetry] = useState(0);
  const storageError = useStore(store, (s) => s.storageError);
  const format =
    catalog?.harness === harness && catalog.taskId === route.taskId
      ? catalog.formats.find((f) => f.kind === kind)
      : undefined;
  const legacy =
    kind === "task" && harness === task?.harness
      ? forms[`${key}:text`]
      : undefined;
  const text = forms[`${previewKey}:text`] ?? legacy,
    name =
      forms[`${previewKey}:name`] ??
      format?.suggested_name ??
      "reusable-task.md";
  const rootId = forms[`${key}:root`] ?? "",
    eligible = roots.filter((r) => r.kind === "drafts" && r.writable),
    root = eligible.find((r) => r.root_id === rootId);
  useEffect(() => {
    if (!online || !grants?.library_read || !grants.tasks) return;
    let alive = true;
    setError(null);
    void session
      .query({ kind: "reusable_formats", task_id: route.taskId, harness })
      .then((r) => {
        if (alive)
          setCatalog({
            harness,
            taskId: route.taskId,
            version: r.native_version,
            formats: r.formats,
          });
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
    grants?.library_read,
    grants?.tasks,
    route.taskId,
    harness,
    profile.name,
    retry,
  ]);
  useEffect(() => {
    if (format?.text != null && text === undefined)
      store.form(`${previewKey}:text`, format.text);
    if (format?.suggested_name && forms[`${previewKey}:name`] === undefined)
      store.form(`${previewKey}:name`, format.suggested_name);
    if (task && forms[`${key}:harness`] === undefined)
      store.form(`${key}:harness`, task.harness);
  }, [format, text, store, previewKey, forms, key, task]);
  const save = async () => {
    if (
      !root ||
      text === undefined ||
      format?.availability.state !== "available"
    )
      return;
    setBusy(true);
    setError(null);
    try {
      const receipt = await session.command(
        {
          command: "create_reusable_draft",
          task_id: route.taskId,
          root_id: root.root_id,
          path: name,
          text,
        },
        { label: `Save draft ${name}` },
      );
      const result = await commandResult(session, receipt);
      if (result?.kind !== "draft_created")
        throw new Error(
          "The computer hasn’t confirmed the draft. Check the destination before trying again.",
        );
      announce("Draft saved in Library.");
      ui.push({ screen: "file", rootId: result.root_id, path: result.path });
    } catch (e) {
      const message =
        e instanceof CommandError && e.error.code === "conflict"
          ? "A file already has this name. Choose a different draft file name; your edited preview is kept."
          : describeError(e, profile.name);
      setError(message);
      announce(message);
    } finally {
      setBusy(false);
    }
  };
  const nameError =
    validName(name) ??
    (!/\.md$/i.test(name)
      ? "Keep the draft’s .md extension until it has been reviewed."
      : /^(AGENTS|CLAUDE|SKILL|GEMINI)\.md$/i.test(name)
        ? "Choose a draft name that agents do not discover automatically."
        : null);
  const allowed =
    online &&
    grants?.library_write &&
    grants.tasks &&
    format?.availability.state === "available";
  return (
    <Screen
      title="Make reusable"
      subtitle={task?.title}
      className="library-screen"
      footerFlows
      footer={
        <div className="lib-footer">
          <Button
            block
            variant="primary"
            pending={busy}
            disabled={!allowed || !root || !text || !!nameError}
            onClick={() => void save()}
          >
            Save reviewed draft
          </Button>
        </div>
      }
    >
      <div className="lib-stack">
        <ConnectionNote />
        <StorageNote />
        <Notice title="Review before saving">
          Edit the preview, then save in Drafts. Saving doesn’t activate it.
        </Notice>
        <label className="lib-root-select">
          Agent
          <select
            value={harness}
            disabled={busy}
            onChange={(e) => store.form(`${key}:harness`, e.target.value)}
          >
            {!harnesses.some((h) => h.kind === harness) && (
              <option value={harness}>Task’s agent · version unknown</option>
            )}
            {harnesses.map((h) => (
              <option key={h.kind} value={h.kind}>
                {h.display_name}
              </option>
            ))}
          </select>
        </label>
        <label className="lib-root-select">
          Reusable kind
          <select
            value={kind}
            disabled={busy}
            onChange={(e) => store.form(`${key}:kind`, e.target.value)}
          >
            {Object.entries(names).map(([k, n]) => (
              <option key={k} value={k}>
                {n}
              </option>
            ))}
          </select>
        </label>
        {catalog?.harness === harness && catalog.taskId === route.taskId && (
          <p className="lib-meta">
            Installed version: {catalog.version ?? "unknown"}
          </p>
        )}
        {format && (
          <>
            <CapabilityNote
              what={names[kind]}
              availability={format.availability}
            />
            {format.instructions && <Notice>{format.instructions}</Notice>}
            {format.documentation && (
              <Button
                variant="quiet"
                onClick={() => {
                  if (format.documentation)
                    void platform
                      .openExternal(format.documentation)
                      .catch((e) => setError(describeError(e, profile.name)));
                }}
              >
                Native format documentation
              </Button>
            )}
          </>
        )}
        {(format?.availability.state === "available" || text !== undefined) && (
          <>
            {!eligible.length ? (
              <Empty title="No writable Drafts folder">
                Configure a writable Drafts folder in {PRODUCT_NAME} on the
                computer.
              </Empty>
            ) : (
              <label className="lib-root-select">
                Draft destination
                <select
                  value={rootId}
                  onChange={(e) => store.form(`${key}:root`, e.target.value)}
                >
                  <option value="">Choose a Drafts folder</option>
                  {eligible.map((r) => (
                    <option key={r.root_id} value={r.root_id}>
                      {r.label}
                    </option>
                  ))}
                </select>
              </label>
            )}
            {root && (
              <p className="lib-path mono">
                {root.host_path} / {name}
              </p>
            )}
            <TextField
              label="Draft file name"
              value={name}
              onChange={(v) => store.form(`${previewKey}:name`, v)}
              error={nameError}
            />
            {text !== undefined && (
              <TextArea
                className="lib-template"
                label="Editable preview"
                value={text}
                onChange={(v) => store.form(`${previewKey}:text`, v)}
                rows={12}
                maxRows={24}
                hint="Edits stay on this device, bound to this task, agent and format."
              />
            )}
            {text !== undefined && !storageError && (
              <StateLine phase="saved_on_phone" />
            )}
          </>
        )}
        {!format && (
          <Button disabled={!online} onClick={() => setRetry((n) => n + 1)}>
            Load supported formats
          </Button>
        )}
        {!grants?.library_write && (
          <Notice>Library writing is not granted to this device.</Notice>
        )}
        {error && <Notice tone="unknown">{error}</Notice>}
      </div>
    </Screen>
  );
}
