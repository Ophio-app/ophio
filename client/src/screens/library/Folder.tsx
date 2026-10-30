// SPDX-License-Identifier: Apache-2.0
import { commandResult } from "../../host/commandResult";
import { useState } from "react";
import {
  FileText,
  Folder as FolderIcon,
  MoreHorizontal,
  Plus,
} from "lucide-react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import type { DirEntry, FileContent, LibraryTrashItem } from "../../protocol/types";
import { useStore } from "../../state/store";
import { Screen, useRoute } from "../../shell/Screen";
import { Button, IconButton } from "../../design/Button";
import { TextField } from "../../design/Field";
import { Empty, ListRow } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { Age } from "../../design/Age";
import { announce } from "../../design/Announcer";
import { toast } from "../../design/Toast";
import { basename, bytes, join, validName } from "./model";
import {
  Breadcrumbs,
  ConnectionNote,
  LoadError,
  StorageNote,
  useFolder,
  useLibrary,
  useScreenBody,
  useRoots,
} from "./shared";
import type { RouteProps } from "../types";
import "./library.css";
import { useWarning } from "../../app/warnings";
import { FolderPicker } from "./FolderPicker";
import { NotAllowed } from "../../shell/NotAllowed";

/** Save backups the computer keeps beside a file, under their current name
 * and the one used before the product was named. Never listed. */
const RESERVED = [".ophio-save-", ".companion-save-"];

type Operation =
  | { kind: "new" }
  | { kind: "actions"; entry: DirEntry }
  | { kind: "rename" | "delete" | "move"; entry: DirEntry; base: FileContent };
export default function Folder({ route }: RouteProps<"folder">) {
  const bodyRef = useScreenBody();
  const { tab: originTab } = useRoute();
  const { ui, session, profile } = useApp(),
    store = useLibrary(),
    link = useLink(),
    roots = useRoots();
  const root = roots.find((r) => r.root_id === route.rootId),
    q = useFolder(route.rootId, route.path),
    grants = useHost((s) => s.grants);
  const [hidden, setHidden] = useState(false),
    [sort, setSort] = useState("name"),
    [destination, setDestination] = useState<string | null>(null),
    [op, setOp] = useState<Operation | null>(null),
    [busy, setBusy] = useState(false),
    [error, setError] = useState<string | null>(null),
    [result, setResult] = useState<string | null>(null);
  const warn = useWarning("file.delete");
  const nameKey = `name:${route.rootId}:${route.path}:${op?.kind ?? "new"}`,
    name = useStore(store, (s) => s.forms[nameKey] ?? "");
  const online = link.state === "online",
    writable = !!root?.writable && !!grants?.library_write;
  const entries = (q.cached?.value.entries ?? [])
    .filter((e) => !RESERVED.some((prefix) => e.name.startsWith(prefix)))
    .filter((e) => !e.hidden || hidden)
    .sort((a, b) =>
      a.kind === "directory" && b.kind !== "directory"
        ? -1
        : b.kind === "directory" && a.kind !== "directory"
          ? 1
          : sort === "modified"
            ? (b.modified_at ?? 0) - (a.modified_at ?? 0)
            : a.name.localeCompare(b.name),
    );
  const start = (next: Operation) => {
    setOp(next);
    setDestination(null);
    setError(null);
  };
  const prepare = async (
    kind: "rename" | "delete" | "move",
    entry: DirEntry,
    /** Go ahead without showing the step's page: a delete without its warning. */
    now = false,
  ) => {
    setBusy(true);
    setError(null);
    try {
      const { file } = await session.query({
        kind: "library_read",
        root_id: route.rootId,
        path: entry.path,
      });
      store.form(`name:${route.rootId}:${route.path}:${kind}`, entry.name);
      if (now) await run({ kind, entry, base: file });
      else start({ kind, entry, base: file });
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setBusy(false);
    }
  };
  const execute = () => run(op);
  const run = async (op: Operation | null) => {
    if (!op || op.kind === "actions") return;
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      const receipt = await session.command(
        op.kind === "new"
          ? {
              command: "library_create",
              root_id: route.rootId,
              path: join(route.path, name),
              text: "",
            }
          : op.kind === "rename" || op.kind === "move"
            ? {
                command: "library_rename",
                root_id: route.rootId,
                from: op.entry.path,
                to:
                  op.kind === "move"
                    ? join(destination ?? route.path, op.entry.name)
                    : join(route.path, name),
                base_revision: op.base.revision,
              }
            : {
                command: "library_delete",
                root_id: route.rootId,
                path: op.entry.path,
                base_revision: op.base.revision,
              },
        {
          label:
            op.kind === "new"
              ? `Create ${name}`
              : `${op.kind === "rename" ? "Rename" : op.kind === "move" ? "Move" : "Delete"} ${op.entry.path}`,
        },
      );
      const result = await commandResult(session, receipt);
      if (op.kind === "rename" || op.kind === "move")
        store.moveFile(route.rootId, op.entry.path,
          op.kind === "move" ? join(destination ?? route.path, op.entry.name) : join(route.path, name));
      if (op.kind === "new" && result?.kind !== "created") {
        setError(
          "The computer hasn’t confirmed file creation. Refresh the folder before trying again.",
        );
        return;
      }
      if (op.kind === "delete") {
        if (result?.kind !== "library_deleted") {
          setError(
            "The computer hasn’t confirmed a recoverable deletion. Refresh before trying again.",
          );
          return;
        }
        // The trash keeps it, so the delete can be taken back from here.
        const item = result.item;
        setOp(null);
        q.refresh();
        toast(`Deleted ${op.entry.name}`, { action: { label: "Undo", run: () => void undo(item) } });
        return;
      }
      const message =
        op.kind === "new"
          ? `Created ${name}`
          : op.kind === "rename"
            ? `Renamed to ${name}`
            : `Moved ${op.entry.name} to ${destination || "the root folder"}`;
      setResult(message);
      announce(message);
      setOp(null);
      q.refresh();
      if (result?.kind === "created") {
        store.form(nameKey, "");
        // A late reply must preserve navigation away from this folder.
        if (ui.get().tab === originTab && ui.route() === route)
          ui.push({ screen: "file", rootId: route.rootId, path: result.path, edit: true }, originTab);
      }
    } catch (e) {
      setError(
        session.online
          ? describeError(e, profile.name)
          : "The computer hasn’t confirmed this change. Reconnect and refresh the folder before trying again.",
      );
    } finally {
      setBusy(false);
    }
  };
  const undo = async (item: LibraryTrashItem) => {
    const name = basename(item.path);
    try {
      const receipt = await session.command(
        { command: "library_restore", trash_id: item.trash_id, base_revision: item.revision, copy_path: null },
        { label: `Restore ${item.path}` },
      );
      const result = await commandResult(session, receipt);
      if (result?.kind !== "library_restored") throw new Error("The computer hasn’t confirmed it. Check Recently deleted.");
      toast(`Restored ${name}`);
      q.refresh();
    } catch (e) {
      toast(`Couldn’t restore ${name}. ${describeError(e, profile.name)}`, { tone: "stop" });
    }
  };
  return (
    <Screen
      bodyRef={bodyRef}
      className="library-screen"
      title={route.path ? basename(route.path) : (root?.label ?? "Folder")}
    >
      <div className="lib-stack">
        {route.path && <Breadcrumbs
          rootId={route.rootId}
          label={root?.label ?? "Root"}
          path={route.path}
        />}
        <ConnectionNote at={q.cached?.at} />
        <StorageNote />
        {!q.allowed && (
          <NotAllowed what="read the Library" permission="library-read" />
        )}
        {q.error && <LoadError error={q.error} retry={q.refresh} />}
        {result && (
          <p role="status" className="lib-meta">
            {result}
          </p>
        )}
        <details className="lib-options">
          <summary>Folder options</summary>
          {root && (
            <p className="lib-path mono">
              {root.host_path}
              {route.path && ` / ${route.path}`}
            </p>
          )}
          <div className="lib-toolbar">
            <label className="lib-check">
              <input
                type="checkbox"
                checked={hidden}
                onChange={(e) => setHidden(e.target.checked)}
              />
              Show hidden files
            </label>
            <label className="lib-sort">
              Sort
              <select value={sort} onChange={(e) => setSort(e.target.value)}>
                <option value="name">Name</option>
                <option value="modified">Modified</option>
              </select>
            </label>
          </div>
          {q.cached && (
            <p className="lib-meta">
              {entries.length} {entries.length === 1 ? "item" : "items"} · Checked{" "}
              <Age at={q.cached.at} suffix=" ago" />
            </p>
          )}
        </details>
        {q.loading && !q.cached && <p role="status">Loading folder…</p>}
        <div className="group">
          {entries.map((e) => (
            <div className="lib-entry" key={e.path}>
              <ListRow
                icon={e.kind === "directory" ? FolderIcon : FileText}
                title={e.name}
                subtitle={
                  e.description ??
                  (e.kind === "directory"
                    ? "Folder"
                    : `${bytes(e.size)}${e.hidden ? " · Hidden" : ""}`)
                }
                onClick={() =>
                  ui.push({
                    screen: e.kind === "directory" ? "folder" : "file",
                    rootId: route.rootId,
                    path: e.path,
                  })
                }
                chevron
              />
              <IconButton
                label={`Actions for ${e.name}`}
                icon={MoreHorizontal}
                onClick={() => start({ kind: "actions", entry: e })}
              />
            </div>
          ))}
        </div>
        {q.cached && !entries.length && (
          <Empty icon={FolderIcon} title="No files here">
            {hidden
              ? "This folder is empty."
              : "Hidden files are off. Show hidden files or create a file."}
          </Empty>
        )}
        {writable ? (
          <Button
            icon={Plus}
            disabled={!online}
            onClick={() => start({ kind: "new" })}
          >
            New file
          </Button>
        ) : (
          <Notice title="Read only">
            {root?.writable
              ? "This device isn’t granted Library writes."
              : "This folder is shared for reading."}
          </Notice>
        )}
        <Button
          variant="quiet"
          onClick={() => ui.push({ screen: "recently_deleted" })}
        >
          Recently deleted
        </Button>
        <p className="lib-meta">New folders must be created on the computer.</p>
      </div>
      <Sheet
        className="library-sheet"
        open={op !== null}
        onClose={() => {
          if (!busy) setOp(null);
        }}
        title={
          op?.kind === "new"
            ? "New file"
            : op?.kind === "move"
              ? "Move file"
              : op?.kind === "rename"
                ? "Rename file"
                : op?.kind === "delete"
                  ? "Delete file?"
                  : (op?.entry.name ?? "File actions")
        }
        subtitle={
          op && "entry" in op ? op.entry.path : route.path || root?.label
        }
        footer={
          op && op.kind !== "actions" ? (
            <Button
              block
              variant={op.kind === "delete" ? "warn" : "primary"}
              pending={busy}
              disabled={
                !online ||
                !writable ||
                (op.kind === "move"
                  ? destination === null || destination === route.path
                  : op.kind !== "delete" && !!validName(name))
              }
              onClick={() => { if (op.kind === "delete") warn.agreed(); void execute(); }}
            >
              {busy
                ? "Waiting for the computer…"
                : op.kind === "new"
                  ? "Create file"
                  : op.kind === "move"
                    ? "Move file"
                    : op.kind === "rename"
                      ? "Rename file"
                      : `Delete ${op.entry.name}`}
            </Button>
          ) : undefined
        }
      >
        <div className="lib-stack">
          {op?.kind === "actions" ? (
            <>
              {op.entry.kind === "directory" ? (
                <Notice>
                  Folder renaming and deletion aren’t available from this device.
                </Notice>
              ) : (
                <>
                  <Button
                    disabled={!online || !writable || busy}
                    onClick={() => void prepare("rename", op.entry)}
                  >
                    Rename…
                  </Button>
                  <Button
                    disabled={!online || !writable || busy}
                    onClick={() => void prepare("move", op.entry)}
                  >
                    Move…
                  </Button>
                  <Button
                    variant="warn"
                    disabled={!online || !writable || busy}
                    onClick={() => warn.ask(() => void prepare("delete", op.entry), () => void prepare("delete", op.entry, true))}
                  >
                    Delete{warn.more}
                  </Button>
                </>
              )}
              {!writable && <Notice>This location is read only.</Notice>}
            </>
          ) : op?.kind === "delete" ? (
            <>
              <p>
                Delete{" "}
                <strong className="lib-path">
                  {root?.label} › {op.entry.path}
                </strong>{" "}
                on the computer?
              </p>
              <Notice tone="warn">
                This moves the file to the computer’s Library trash. You can
                restore it from Recently deleted. Any draft on this device is kept.
              </Notice>
              {warn.check}
            </>
          ) : op?.kind === "move" ? (
            <>
              <FolderPicker
                rootId={route.rootId}
                initial={route.path}
                onChoose={setDestination}
              />
              {destination !== null && (
                <Notice>
                  Move {op.entry.name} to {destination || "the root folder"}.
                  Existing files are never replaced.
                </Notice>
              )}
              <p className="lib-meta">
                Moves stay within this root. Cross-root moves are unavailable.
                This device’s draft follows the file once the computer confirms the move.
              </p>
            </>
          ) : (
            <>
              <TextField
                label="File name"
                value={name}
                onChange={(v) => store.form(nameKey, v)}
                error={name ? validName(name) : null}
                hint="Use .md for Markdown. Existing files are never replaced."
              />
              {op?.kind === "rename" && (
                <Notice>
                  Renaming can break links or change how agents find this file.
                  This device’s draft follows the file once the computer confirms the rename.
                </Notice>
              )}
            </>
          )}
          {error && <Notice tone="stop">{error}</Notice>}
        </div>
      </Sheet>
    </Screen>
  );
}
