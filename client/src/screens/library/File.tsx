// SPDX-License-Identifier: Apache-2.0
import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { Download, Pencil, Star } from "lucide-react";
import { describeError, useApp, useHost, useLink, useUi } from "../../app/context";
import { useStore } from "../../state/store";
import { Screen, useRoute } from "../../shell/Screen";
import { useTightLayout } from "../../shell/keyboard";
import { Button, IconButton } from "../../design/Button";
import { Segmented } from "../../design/Chip";
import { Notice } from "../../design/Notice";
import { Empty } from "../../design/ListRow";
import { Markdown } from "../../design/Markdown";
import { Sheet } from "../../design/Sheet";
import { StateLine } from "../../design/StateLine";
import { Age } from "../../design/Age";
import { toast } from "../../design/Toast";
import {
  basename,
  bytes,
  category,
  dirname,
  isMarkdown,
  lf,
  READ_LIMIT,
} from "./model";
import { draftStart, fileKey } from "./drafts";
import {
  Breadcrumbs,
  ConnectionNote,
  Diff,
  LoadError,
  StorageNote,
  useFile,
  useFolder,
  useLibrary,
  useScreenBody,
  useRoots,
  useAgentFiles,
  cachedAgentFiles,
} from "./shared";
import { DisableControl } from "./Disabled";
import { listedDisabled, useDisabled } from "../../app/disabled";
import { mergeMarkersLeft } from "./diff";
import { saveDraft, checkSave, compareWithComputer } from "./save";
import { transferManager } from "../transfers/manager";
import type { RouteProps } from "../types";
import { NotAllowed } from "../../shell/NotAllowed";
import { FileProvenance, readerContext } from "./Provenance";
import { MovedDrafts } from "./MovedDrafts";
import { UncheckedSaves } from "./UncheckedSaves";
import "./library.css";
import { useWarning } from "../../app/warnings";
import { InlineCode } from "../../design/InlineCode";
const Editor = lazy(() => import("./Editor"));

export default function File({ route }: RouteProps<"file">) {
  const tight = useTightLayout();
  const bodyRef = useScreenBody();
  const app = useApp(),
    { ui, session, profile, platform } = app,
    link = useLink(),
    store = useLibrary(),
    roots = useRoots(),
    { active, tab, depth } = useRoute();
  const parentRoute = useUi(s => s.stacks[tab][depth - 1]);
  route = readerContext(route, parentRoute);
  const root = roots.find((r) => r.root_id === route.rootId),
    folder = useFolder(route.rootId, dirname(route.path));
  const entry = folder.cached?.value.entries.find((e) => e.path === route.path),
    markdownFile = isMarkdown(route.path),
    tooLarge = (entry?.size ?? 0) > READ_LIMIT;
  const q = useFile(route.rootId, route.path, markdownFile && !tooLarge),
    file = q.cached?.value;
  const draft = useStore(
      store,
      (s) => s.drafts[fileKey(route.rootId, route.path)],
    ),
    favourite = useStore(store, (s) =>
      s.favourites.some(
        (r) => r.rootId === route.rootId && r.path === route.path,
      ),
    );
  const grants = useHost((s) => s.grants);
  // An agent file can be disabled in Øphio, here as in its list.
  const possibleAgentFile = root?.kind === "agent_files" ||
    /^(SKILL|CLAUDE|AGENTS|CLAUDE\.local|AGENTS\.override|MEMORY)\.md$/i.test(basename(route.path)) ||
    /(?:^|\/)(?:\.claude|\.agents|\.codex)\//.test(route.path);
  const scanned = useAgentFiles(possibleAgentFile);
  const agentFile = (possibleAgentFile ? scanned.cached : cachedAgentFiles(profile.profileId))?.files.find(
    (f) => f.root_id === route.rootId && f.path === route.path,
  );
  const disabled = useDisabled();
  const off = !!agentFile?.host_path && disabled.supported && disabled.isDisabled(agentFile.disable?.state === "can" ? agentFile.disable.key ?? agentFile.host_path : agentFile.host_path, listedDisabled(agentFile.disable), agentFile.disable?.state === "can" && !!agentFile.disable.key);
  const storageError = useStore(store, (s) => s.storageError);
  const [view, setView] = useState<"rendered" | "source">("rendered"),
    [editing, setEditing] = useState(false),
    [review, setReview] = useState(false),
    [closing, setClosing] = useState(false),
    [discard, setDiscard] = useState(false),
    [busy, setBusy] = useState(false),
    [composing, setComposing] = useState(false),
    [saved, setSaved] = useState(false),
    [message, setMessage] = useState<string | null>(null),
    [error, setError] = useState<string | null>(null);
  const warn = useWarning("draft.discard");
  const online = link.state === "online",
    unknown = draft?.phase === "unknown",
    sending = draft?.phase === "sending",
    conflict = draft?.phase === "conflict",
    writable = root?.writable && grants?.library_write !== false;
  // After a conflict the reader shows what the computer reported, not the
  // version the draft started from.
  const display = conflict ? (draft.current ?? undefined) : (file ?? draft?.base);
  const editable =
    markdownFile &&
    !!display &&
    !tooLarge &&
    display.size <= READ_LIMIT &&
    display.encoding === "utf8" &&
    display.newline !== "mixed" &&
    !draft?.movedTo &&
    writable;
  const changed =
    // A change event can precede our save receipt (including an uncertain one).
    // Compare it with the resulting revision only after the save is resolved.
    !sending && !unknown &&
    q.latest !== undefined &&
    q.latest !==
      (conflict
        ? (draft.current?.revision ?? null)
        : (draft?.base.revision ?? display?.revision));
  useEffect(() => {
    if (display && active && !draft?.movedTo) store.remember(route.rootId, route.path);
  }, [display?.revision, active, store, route.rootId, route.path, draft?.movedTo]);
  useEffect(() => {
    if (active && !draft?.movedTo && (draft?.reviewMerge || draft?.reviewRecovery)) {
      setEditing(true);
    }
  }, [active, draft?.movedTo, draft?.reviewMerge, draft?.reviewRecovery]);
  const edit = () => {
    if (!display || !editable) return;
    if (!draft)
      store.saveDraft({
        base: display,
        text: lf(display.text),
        updatedAt: Date.now(),
        startedAt: draftStart(),
        phase: "draft",
      });
    setEditing(true);
    setSaved(false);
    setError(null);
  };
  // A file made to be edited, a new one or a customized copy, opens in the
  // editor once it has loaded. Only the first time: closing it stays closed.
  const opened = useRef(!route.edit);
  useEffect(() => {
    if (opened.current || !active || !editable) return;
    opened.current = true;
    edit();
  });
  const leaveEditor = () => {
    if (draft?.phase === "draft" && lf(draft.text) === lf(draft.base.text)) {
      store.discardDraft(route.rootId, route.path);
      setEditing(false);
    } else if (draft) setClosing(true);
    else setEditing(false);
  };
  const keep = () => {
    setClosing(false);
    setEditing(false);
    if (draft?.reviewMerge || draft?.reviewRecovery) store.saveDraft({ ...draft, reviewMerge: false, reviewRecovery: undefined });
  };
  const resolveConflict = () => {
    setEditing(false);
    setReview(false);
    ui.push({ screen: "conflict", rootId: route.rootId, path: route.path });
  };
  const handleResult = (r: Awaited<ReturnType<typeof checkSave>>) => {
    if (r.kind === "rebased") {
      setSaved(false);
      setReview(false);
      setMessage("The earlier save is confirmed. Your newer edits are kept on this device. Review and save them when ready.");
    }
    if (r.kind === "saved") {
      setSaved(true);
      setReview(false);
      setEditing(false);
      setMessage(null);
    }
    if (r.kind === "conflict") resolveConflict();
    if (r.kind === "unknown") setReview(false);
    if (r.kind === "unchanged")
      setMessage(
        "The computer still has the original version. Review and save your draft when ready.",
      );
  };
  const save = async (check = false) => {
    if (!draft) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      handleResult(
        check
          ? await checkSave(session, store, draft)
          : await saveDraft(session, store, draft),
      );
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setBusy(false);
    }
  };
  const compare = async () => {
    if (!draft) return;
    setBusy(true);
    setError(null);
    try {
      if ((await compareWithComputer(session, store, draft)) === "conflict") resolveConflict();
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setBusy(false);
    }
  };
  const sendToPhone = () => {
    transferManager(app).startDownload(
      { kind: "library_file", root_id: route.rootId, path: route.path },
      basename(route.path),
    );
    ui.push({ screen: "transfers" });
  };
  const unknownNotice = unknown && (
    <Notice
      tone="unknown"
      title="The computer hasn’t confirmed this save"
      action={
        <Button
          disabled={!online || busy}
          pending={busy}
          onClick={() => void save(true)}
        >
          Check again
        </Button>
      }
    >
      Your draft is kept on this device.
      {draft.recoveryId && (
        <span className="lib-path mono">Recovery ID: {draft.recoveryId}</span>
      )}
    </Notice>
  );
  // Opening the editor starts a draft; nothing is kept until it changes.
  const untouched = draft?.phase === "draft" && lf(draft.text) === lf(draft.base.text);
  const status =
    draft && storageError ? (
      <p className="lib-meta">Draft held in memory · storage unavailable</p>
    ) : untouched ? (
      <p className="lib-meta">No changes yet</p>
    ) : draft ? (
      <StateLine
        phase={sending ? "sending" : unknown ? "unknown" : "saved_on_phone"}
        at={sending || unknown ? draft.saveAttemptAt : draft.updatedAt}
      />
    ) : saved ? (
      <StateLine phase="saved_on_computer" />
    ) : null;
  const changedNotice = changed && (
    <Notice
      tone="warn"
      title={
        q.latest === null
          ? "Deleted on the computer"
          : "Changed on the computer since you opened it"
      }
      action={
        draft ? (
          <Button disabled={!online || busy} pending={busy} onClick={() => void compare()}>
            Compare with the computer’s
          </Button>
        ) : q.latest !== null ? (
          <Button disabled={!online || q.loading} pending={q.loading} onClick={q.refresh}>
            Show the latest
          </Button>
        ) : undefined
      }
    >
      {draft ? "Your draft starts from an earlier version. Compare the two before you save; your draft stays on this device."
        : "Showing the version last read on this device."}
    </Notice>
  );
  const conflictNotice = (reading: boolean) => conflict && (
    <Notice
      tone="warn"
      title={
        draft.current
          ? "Conflict · changed on the computer"
          : "Conflict · deleted on the computer"
      }
      action={<Button onClick={resolveConflict}>Review conflicting changes</Button>}
    >
      {draft.current
        ? `${reading ? "Showing what is on the computer now. " : ""}Your draft is kept on this device and starts from an earlier version.`
        : "The file is no longer on the computer. Your draft is kept on this device."}
    </Notice>
  );
  const offlineReason = !online && (
    <p className="lib-meta">
      {link.state === "failed" ? link.detail : "Connect to the computer to save."} Editing stays on this device.
    </p>
  );
  const markersLeft =
    !!draft && mergeMarkersLeft(draft.text, draft.base.text, draft.mergeMarkers === true);
  const canSave =
    !!draft &&
    online &&
    grants?.library_write === true &&
    !unknown &&
    !sending &&
    !draft.movedTo &&
    writable &&
    !composing &&
    !markersLeft;
  const writeAccessNote = grants?.library_write === false ? (
    <NotAllowed what="save Library changes" permission="library-write"
      detail="Library write access is unavailable. Your draft stays on this device." />
  ) : root?.writable === false ? (
    <Notice title="This Library folder is read only">Your draft stays on this device. Ask the computer’s owner to allow writes to this folder.</Notice>
  ) : null;
  const editorFooter = (
    <div className="lib-stack">
      {status}
      {offlineReason}
      {writeAccessNote}
      {markersLeft && <p className="lib-meta">Remove the conflict markers to save.</p>}
      <Button
        block
        variant="primary"
        disabled={!canSave || busy || untouched}
        onClick={() => setReview(true)}
      >
        Review changes
      </Button>
    </div>
  );
  return (
    <Screen
      bodyRef={bodyRef}
      className="library-screen"
      title={basename(route.path)}
      // Many files share a name, SKILL.md or README.md; the folder says which this is.
      subtitle={root && `${root.label}${dirname(route.path) ? ` › ${dirname(route.path)}` : ""}`}
      actions={
        <IconButton
          icon={Star}
          label={favourite ? "Remove favourite" : "Add favourite"}
          pressed={favourite}
          onClick={() => store.favourite(route.rootId, route.path)}
        />
      }
      footerFlows
      footer={
        editable && !editing ? (
          <div className="lib-footer">
            <Button block variant="primary" icon={Pencil} onClick={edit}>
              {draft ? "Continue editing draft" : "Edit"}
            </Button>
          </div>
        ) : undefined
      }
    >
      <div className="lib-stack">
        <details className="lib-options">
          <summary>File details</summary>
          <Breadcrumbs
            rootId={route.rootId}
            label={root?.label ?? "Root"}
            path={dirname(route.path)}
          />
          <div className="lib-identity">
            <p className="lib-path mono">{root?.host_path}{dirname(route.path) ? ` / ${dirname(route.path)}` : ""}</p>
            <p className="lib-meta">
              {bytes(display?.size ?? entry?.size ?? 0)}
              {display && (
                <>
                  {" "}
                  · Revision <InlineCode>{display.revision.slice(0, 10)}</InlineCode>
                </>
              )}
              {display?.modified_at != null && (
                <>
                  {" "}
                  · Modified <Age at={display.modified_at} suffix=" ago" />
                </>
              )}
            </p>
            {display && (
              <p className="lib-meta">
                {display.newline === "cr_lf"
                  ? "CRLF"
                  : display.newline === "mixed"
                    ? "Mixed line endings"
                    : "LF"}
                {display.has_bom ? " · UTF-8 with BOM" : " · UTF-8"}
              </p>
            )}
          </div>
          {display && markdownFile && (
            <Segmented
              label="Document view"
              value={view}
              options={[
                { value: "rendered", label: "Rendered" },
                { value: "source", label: "Source" },
              ]}
              onChange={setView}
            />
          )}
        </details>
        <ConnectionNote at={q.cached?.at ?? folder.cached?.at} />
        <FileProvenance route={route} revision={display?.revision} />
        {off && (
          <Notice title="Disabled in Øphio">
            Agents that Øphio runs don’t see this file. It isn’t changed, and agents used outside Øphio still have it.
          </Notice>
        )}
        <StorageNote />
        {!q.allowed && (
          <NotAllowed what="read the Library" permission="library-read" />
        )}
        {q.error && <LoadError error={q.error} retry={q.refresh} />}
        {status}
        {conflictNotice(true)}
        {changedNotice}
        {unknownNotice}
        <UncheckedSaves rootId={route.rootId} path={route.path} />
        <MovedDrafts rootId={route.rootId} path={route.path} />
        {draft && !editing && !unknown && !conflict && (
          <Notice title="This device has a draft">
            Based on revision <InlineCode>{draft.base.revision.slice(0, 10)}</InlineCode> ·
            Edited <Age at={draft.updatedAt} suffix=" ago" />.
            <div className="lib-actions">
              <Button onClick={edit} disabled={!editable}>
                Open draft
              </Button>
              <Button variant="quiet" onClick={() => warn.ask(() => setDiscard(true), () => {
                // A save still being checked keeps its warning, which says so.
                if (draft.phase === "sending" || draft.phase === "unknown") setDiscard(true);
                else store.discardDraft(route.rootId, route.path);
              })}>
                Discard draft{warn.more}
              </Button>
            </div>
          </Notice>
        )}
        {message && <Notice>{message}</Notice>}
        {error && <Notice tone="stop">{error}</Notice>}
        {!markdownFile ? (
          <Empty title="File details">
            This file isn’t Markdown. Send it to your device to open it in
            another app.
          </Empty>
        ) : tooLarge || (display && display.size > READ_LIMIT) ? (
          <Notice title="Large file · read only">
            This file is larger than the editor’s {bytes(READ_LIMIT)}{" "}
            limit on this device. Send it to your device to read the full file.
          </Notice>
        ) : display?.encoding === "unsupported" ? (
          <Notice title="Unsupported text encoding">
            Send the original file to your device. Editing here could change its
            bytes.
          </Notice>
        ) : display ? (
          <>
            {view === "rendered" ? (
              <article className="lib-document">
                <Markdown
                  document
                  text={display.text}
                  onLink={(href) => {
                    if (/^https?:\/\//i.test(href))
                      void platform
                        .openExternal(href)
                        .catch((e) => toast(describeError(e, profile.name)));
                    else toast("Open linked files from their Library folder.");
                  }}
                />
              </article>
            ) : (
              <pre className="lib-source">{display.text}</pre>
            )}
            {display.newline === "mixed" && (
              <Notice title="Mixed line endings · read only">
                Edit this file on the computer to preserve its original
                formatting.
              </Notice>
            )}
            {!writable && (
              <Notice title="Read only">
                {root?.writable
                  ? "This device isn’t granted Library writes."
                  : "This folder is shared for reading only."}
              </Notice>
            )}
            {!route.taskId && category(route.path, root?.kind) !== "Documents" && (
              <p className="lib-meta">
                {category(route.path, root?.kind)} · Øphio can’t tell whether a
                running agent has read this version.
              </p>
            )}
          </>
        ) : (
          !conflict &&
          !q.error &&
          q.allowed && (
            <p role="status">
              {online
                ? "Reading file…"
                : "This file hasn’t been read on this device."}
            </p>
          )
        )}
        <Button
          icon={Download}
          disabled={!online || !grants?.transfers}
          onClick={sendToPhone}
        >
          Send to this device
        </Button>
        {agentFile?.host_path && (
          <DisableControl
            disable={agentFile.disable}
            target={{
              kind: agentFile.disable?.state === "can" ? agentFile.disable.kind : "file",
              host_path: agentFile.host_path,
              key: agentFile.disable?.state === "can" ? agentFile.disable.key : undefined,
              name: agentFile.title ?? basename(agentFile.shown),
              description: agentFile.description,
              scope: agentFile.scope,
              harness: agentFile.harness,
              project_id: agentFile.project_id,
              group: agentFile.group,
              library: { root_id: agentFile.root_id, path: agentFile.path },
            }}
          />
        )}
      </div>
      <Sheet
        className="library-sheet library-editor-sheet"
        size="full"
        open={editing && active}
        title="Edit Markdown"
        subtitle={tight ? undefined : `${root?.label ?? "Root"} › ${route.path}`}
        closeLabel="Close editor"
        onClose={leaveEditor}
        footer={tight ? undefined : editorFooter}
      >
        <div className="lib-stack lib-editor-content">
          <StorageNote />
          {conflictNotice(false)}
          {changedNotice}
          {unknownNotice}
          {draft?.reviewRecovery && (
            <Notice title="Recovery draft · review before saving">
              The save checks the destination revision you compared. The recovery copy stays on the computer until you explicitly discard it.
            </Notice>
          )}
          {draft?.reviewMerge && (
            <Notice tone="warn" title="Merged draft · review every change">
              {markersLeft
                ? "Overlaps you chose to edit by hand are between markers. Keep the text you want and remove each marker line."
                : "The drafts were merged with your choices. Review the changes before saving."}
            </Notice>
          )}
          {message && <Notice>{message}</Notice>}
          {error && <Notice tone="stop">{error}</Notice>}
          {draft && (
            <Suspense fallback={<p>Opening editor…</p>}>
              <Editor
                initialText={lf(draft.text)}
                onComposing={setComposing}
                onChange={(text) => {
                  const current = store.draft(route.rootId, route.path);
                  if (current && current.phase !== "sending")
                    store.saveDraft({
                      ...current,
                      text,
                      updatedAt: Date.now(),
                      // With the merge's markers gone, a new "=======" underline is the writer's own.
                      ...(current.mergeMarkers && {
                        mergeMarkers: mergeMarkersLeft(text, current.base.text, true),
                      }),
                    });
                }}
              />
            </Suspense>
          )}
          {tight && <>
            <p className="lib-path lib-meta">{root?.label ?? "Root"} › {route.path}</p>
            {editorFooter}
          </>}
        </div>
      </Sheet>
      <Sheet
        className="library-sheet"
        open={review && active}
        size="tall"
        title="Review save"
        subtitle={`${root?.label} › ${route.path}`}
        onClose={() => {
          if (!busy) setReview(false);
        }}
        footer={
          <div className="lib-stack">
          {writeAccessNote}
          <Button
            block
            variant="primary"
            disabled={!canSave}
            pending={busy}
            onClick={() => void save()}
          >
            {busy ? "Waiting for the computer…" : "Save on computer"}
          </Button>
          </div>
        }
      >
        <div className="lib-stack">
          {conflictNotice(false)}
          {changedNotice}
          {draft && (
            <p className="lib-meta">
              {draft.reviewMerge
                ? "Compared with the computer’s version you merged with"
                : draft.reviewRecovery
                  ? "Compared with the computer’s version you reviewed for recovery"
                  : "Compared with the version you opened"}{" "}
              · Revision <InlineCode>{draft.base.revision.slice(0, 10)}</InlineCode>
            </p>
          )}
          {draft && <Diff base={draft.base.text} text={draft.text} />}
          {category(route.path, root?.kind) !== "Documents" && (
            <Notice title="Agent instructions">
              Saving may affect how agents work in this folder. It does not
              confirm that a running agent reloaded this file.
            </Notice>
          )}
          {error && <Notice tone="stop">{error}</Notice>}
          {offlineReason}
        </div>
      </Sheet>
      <Sheet
        className="library-sheet"
        open={(closing || discard) && active}
        title={
          discard
            ? `Discard draft for ${basename(route.path)}?`
            : "Keep your device draft?"
        }
        onClose={() => {
          setClosing(false);
          setDiscard(false);
        }}
        footer={
          <div className="lib-stack">
            <Button
              block
              variant="primary"
              onClick={() => {
                setDiscard(false);
                keep();
              }}
            >
              Keep draft
            </Button>
            <Button
              block
              variant="stop-outline"
              disabled={sending}
              onClick={() => {
                if (discard) warn.agreed();
                store.discardDraft(route.rootId, route.path);
                setClosing(false);
                setDiscard(false);
                setEditing(false);
              }}
            >
              Discard draft for {basename(route.path)}
            </Button>
          </div>
        }
      >
        <p>
          Discarding permanently removes only this device’s draft.
          {unknown ? " The earlier save may already have changed the computer’s file. Its status stays unconfirmed until checked."
            : " It does not undo any save on the computer."}
        </p>
        {discard && warn.check}
      </Sheet>
    </Screen>
  );
}
