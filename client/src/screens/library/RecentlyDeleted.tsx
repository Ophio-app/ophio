// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { commandResult } from "../../host/commandResult";
import type { LibraryTrashItem } from "../../protocol/types";
import { Screen } from "../../shell/Screen";
import { Button } from "../../design/Button";
import { Empty, ListRow } from "../../design/ListRow";
import { TextField } from "../../design/Field";
import { Sheet } from "../../design/Sheet";
import { Notice } from "../../design/Notice";
import { Markdown } from "../../design/Markdown";
import { Age } from "../../design/Age";
import { announce } from "../../design/Announcer";
import { useStore } from "../../state/store";
import { ConnectionNote, useLibrary, useRoots } from "./shared";
import { basename, bytes, dirname, join, validName } from "./model";
import type { RouteProps } from "../types";
import "./library.css";
import { InlineCode } from "../../design/InlineCode";

export default function RecentlyDeleted(
  _props: RouteProps<"recently_deleted">,
) {
  const { session, profile, ui } = useApp(),
    online = useLink().state === "online",
    roots = useRoots(),
    store = useLibrary();
  const writable = useHost((s) => s.grants?.library_write);
  const [items, setItems] = useState<LibraryTrashItem[] | null>(null),
    [selected, setSelected] = useState<LibraryTrashItem | null>(null);
  const [content, setContent] = useState<string | null>(null),
    [error, setError] = useState<string | null>(null),
    [busy, setBusy] = useState(false),
    [retry, setRetry] = useState(0),
    [at, setAt] = useState<number>();
  const key = `restore:${selected?.trash_id ?? ""}`,
    name = useStore(store, (s) => s.forms[key] ?? "");
  useEffect(() => {
    if (!online) return;
    let alive = true;
    void session
      .query({ kind: "library_trash" })
      .then((r) => {
        if (alive) {
          setItems(r.items);
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
  }, [session, online, profile.name, retry]);
  useEffect(() => {
    setContent(null);
    if (!selected || !online) return;
    let alive = true;
    void session
      .query({ kind: "library_trash_read", trash_id: selected.trash_id })
      .then((r) => {
        if (alive) setContent(r.file.text);
      })
      .catch((e: unknown) => {
        if (alive) setError(describeError(e, profile.name));
      });
    return () => {
      alive = false;
    };
  }, [session, online, selected, profile.name]);
  const restore = async (copy: boolean) => {
    if (!selected) return;
    setBusy(true);
    setError(null);
    try {
      const receipt = await session.command(
        {
          command: "library_restore",
          trash_id: selected.trash_id,
          base_revision: selected.revision,
          copy_path: copy ? join(dirname(selected.path), name) : null,
        },
        { label: `Restore ${basename(selected.path)}` },
      );
      const result = await commandResult(session, receipt);
      if (result?.kind !== "library_restored")
        throw new Error(
          "The computer hasn’t confirmed restoration. Check Recently deleted before trying again.",
        );
      announce(`Restored ${result.path}`);
      setSelected(null);
      setRetry((n) => n + 1);
      ui.push({ screen: "file", rootId: result.root_id, path: result.path });
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setBusy(false);
    }
  };
  const canRestore =
    online &&
    writable &&
    roots.some((r) => r.root_id === selected?.root_id && r.writable);
  return (
    <Screen title="Recently deleted" className="library-screen">
      <div className="lib-stack">
        <ConnectionNote at={at} />
        <p className="lib-meta">
          Files in the computer’s Library trash, kept for 30 days. Restoring
          never replaces a file at the destination.
        </p>
        {error && !selected && (
          <Notice
            tone="stop"
            action={
              <Button onClick={() => setRetry((n) => n + 1)}>Try again</Button>
            }
          >
            {error}
          </Notice>
        )}
        {items === null && !error && (
          <p>
            {online
              ? "Loading recoverable files…"
              : "Connect to view the computer’s Library trash."}
          </p>
        )}
        {items?.length === 0 && (
          <Empty title="No recently deleted files">
            Recoverable Library deletions appear here.
          </Empty>
        )}
        <div className="group">
          {items?.map((item) => (
            <ListRow
              key={item.trash_id}
              title={basename(item.path)}
              subtitle={
                <>
                  <span className="lib-path">
                    {roots.find((r) => r.root_id === item.root_id)?.label ??
                      "Library"}{" "}
                    › {item.path}
                  </span>
                  <span>
                    {bytes(item.size)} · Deleted{" "}
                    <Age at={item.deleted_at} suffix=" ago" />
                  </span>
                </>
              }
              onClick={() => {
                setSelected(item);
                setError(null);
              }}
              chevron
            />
          ))}
        </div>
      </div>
      <Sheet
        className="library-sheet"
        size="tall"
        open={!!selected}
        title={selected ? basename(selected.path) : "Deleted file"}
        subtitle={selected?.path}
        onClose={() => {
          if (!busy) setSelected(null);
        }}
        footer={
          <Button
            block
            variant="primary"
            pending={busy}
            disabled={!canRestore}
            onClick={() => void restore(false)}
          >
            Restore original path
          </Button>
        }
      >
        <div className="lib-stack">
          {selected && (
            <p className="lib-meta">
              {bytes(selected.size)} · Revision{" "}
              <InlineCode>{selected.revision.slice(0, 12)}</InlineCode>
            </p>
          )}
          {error && (
            <Notice tone="warn">
              {error} If the original path is taken, choose a different name
              below.
            </Notice>
          )}
          <TextField
            label="Restore as copy name"
            value={name}
            onChange={(v) => store.form(key, v)}
            error={name ? validName(name) : null}
            hint="The copy stays in the original folder. The deleted copy remains in trash."
          />
          <Button
            pending={busy}
            disabled={
              !canRestore ||
              !name ||
              !!validName(name) ||
              name === basename(selected?.path ?? "")
            }
            onClick={() => void restore(true)}
          >
            Restore as copy
          </Button>
          {content !== null && (
            <article className="lib-document">
              <Markdown document text={content} />
            </article>
          )}
        </div>
      </Sheet>
    </Screen>
  );
}
