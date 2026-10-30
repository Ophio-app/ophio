// SPDX-License-Identifier: Apache-2.0
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { Upload } from "lucide-react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { useStore } from "../../state/store";
import { Screen } from "../../shell/Screen";
import { Button } from "../../design/Button";
import { Segmented } from "../../design/Chip";
import { TextArea, TextField } from "../../design/Field";
import { Empty, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { Age } from "../../design/Age";
import { announce } from "../../design/Announcer";
import { transferManager, type TransferJob } from "./manager";
import { chooseComposerFile, composerFileKey, transferComposer } from "./composer";
import { transferBytes } from "./storage";
import { saveToPhone } from "./saveToPhone";
import { DestinationPicker } from "./DestinationPicker";
import { commandResult } from "../../host/commandResult";
import { uuidv7 } from "../../protocol/ids";
import { publicationPath, transferName, TransferRow } from "./TransferRow";
import { bytes, relativeDir, validName } from "../library/model";
import { ConnectionNote, useScreenBody } from "../library/shared";
import type {
  TransferRecord,
  UploadCollision,
  UploadDestinationCheck,
} from "../../protocol/types";
import type { RouteProps } from "../types";
import { NotAllowed } from "../../shell/NotAllowed";
import "./transfers.css";
/** States a transfer is still moving in. */
const MOVING = ["preparing", "pending", "sending", "in_progress", "verifying"];
/** States that wait on the person: continue, check, cancel or start over. */
const WAITING = ["paused", "failed", "unknown", "reconciliation_required"];
/** One transfer: this phone's job with the computer's record of it, or a
 * record alone (another phone's, an app update, a picture shown in a task). */
interface Row {
  key: string;
  job?: TransferJob;
  record?: TransferRecord;
  at: number;
}
function rowPhase(r: Row): string {
  // The computer's record decides a check: nothing on the phone can.
  return r.record?.state.state === "reconciliation_required"
    ? "reconciliation_required"
    : (r.job?.phase ?? r.record?.state.state ?? "unknown");
}
/** Moving, waiting on the person, or a download not yet saved. A record
 * alone waits only for a check: its failures belong to the phone that ran it. */
function current(r: Row): boolean {
  if (r.job?.retried) return false;
  const phase = rowPhase(r);
  if (!r.job) return MOVING.includes(phase) || phase === "reconciliation_required";
  return (
    MOVING.includes(phase) ||
    WAITING.includes(phase) ||
    (r.job.direction === "download" && phase === "completed" && !r.job.savedAs)
  );
}
export default function Transfers(_props: RouteProps<"transfers">) {
  const bodyRef = useScreenBody();
  const currentRef = useRef<HTMLDivElement>(null),
    composeRef = useRef<HTMLDivElement>(null);
  /** Show what was just sent once its row has rendered. */
  const revealCurrent = useRef(false);
  useLayoutEffect(() => {
    if (!revealCurrent.current) return;
    revealCurrent.current = false;
    currentRef.current?.scrollIntoView?.({ block: "start" });
  });
  /** Rows shown as current during this visit stay there as they finish, so
   * one doesn't move into the folded history while it is being watched. */
  const seen = useRef(new Set<string>());
  const historyBody = useId();
  const fileInput = useRef<HTMLInputElement>(null);
  const app = useApp(),
    { profile, session, platform } = app,
    link = useLink(),
    manager = transferManager(app),
    composer = transferComposer(profile.profileId);
  const c = useStore(composer, (s) => s),
    composerRecovery = useStore(composer.recovery, (s) => s),
    jobs = useStore(manager, (s) => s.jobs),
    storageError = useStore(manager, (s) => s.storageError),
    storageRecovery = useStore(manager, (s) => s.storageRecovery),
    live = useHost((s) => s.transfers),
    grants = useHost((s) => s.grants?.transfers),
    projects = useHost((s) => s.projects),
    noInbox = useHost((s) => !!s.host && !s.host.inbox_path);
  const [collision, setCollision] = useState<UploadDestinationCheck | null>(
    null,
  );
  const [checkingDestination, setCheckingDestination] = useState(false);
  const [cancellingId, setCancellingId] = useState<string | null>(null);
  const [history, setHistory] = useState<TransferRecord[]>([]),
    [checkedAt, setCheckedAt] = useState<number | null>(null),
    [error, setError] = useState<string | null>(null),
    // Shown beside Send, where the person who pressed it is looking.
    [sendError, setSendError] = useState<string | null>(null),
    /** Bytes this view holds, and which choice they are; null bytes: none kept. */
    [held, setHeld] = useState<{ fileId?: string; blob: Blob | null } | null>(
      null,
    ),
    /** A file chosen here, shown while its bytes are being kept. */
    [picking, setPicking] = useState<File | null>(null),
    [cancel, setCancel] = useState<{
      id: string;
      local: boolean;
      name: string;
      /** Set when cancelling only acknowledges a checked destination. */
      checking: { where: string | null } | null;
    } | null>(null),
    [busy, setBusy] = useState(false),
    [saving, setSaving] = useState<string | null>(null),
    // Downloads whose verified bytes Save found gone from this device.
    [gone, setGone] = useState<ReadonlySet<string>>(new Set()),
    [showHistory, setShowHistory] = useState(false),
    [historyQuery, setHistoryQuery] = useState("");
  const online = link.state === "online";
  // The bytes held here must be the file the composer names. Another view
  // (the capture inbox, or Transfers in another tab) may have chosen it, and
  // a file with the same name and size can still be different bytes, so they
  // are matched by the choice's identity.
  const holding = useRef(held);
  const hold = (h: typeof held) => {
    holding.current = h;
    setHeld(h);
  };
  const kept = c.fileName && held && held.fileId === c.fileId ? held : null,
    blob = kept?.blob ?? null;
  useEffect(() => {
    const before = holding.current,
      fileId = c.fileId;
    if (!c.fileName || (before && before.fileId === fileId)) return;
    let alive = true;
    void transferBytes
      .get(composerFileKey(profile.profileId, fileId))
      .then((v) => {
        // A file chosen here while this was read is newer than it.
        if (alive && holding.current === before) hold({ fileId, blob: v });
      })
      .catch((e: unknown) => {
        if (alive) setError(describeError(e, profile.name));
      });
    return () => {
      alive = false;
    };
  }, [profile.profileId, profile.name, c.fileName, c.fileId]);
  useEffect(() => {
    if (!online || grants === false) return;
    let alive = true;
    void session
      .query({ kind: "transfers" })
      .then((r) => {
        if (alive) {
          setHistory(r.transfers);
          setCheckedAt(Date.now());
        }
      })
      .catch((e: unknown) => {
        if (alive) setError(describeError(e, profile.name));
      });
    return () => {
      alive = false;
    };
  }, [online, grants, session, profile.name]);
  const records = {
    ...Object.fromEntries(history.map((r) => [r.transfer_id, r])),
    ...live,
  };
  useEffect(() => {
    manager.observe([...Object.values(live), ...history]);
  }, [manager, history, live, jobs]);
  const localJobs = Object.values(jobs),
    known = new Set(localJobs.map((j) => j.transferId));
  const rows: Row[] = [
    ...localJobs.map((j) => ({
      key: j.id,
      job: j,
      record: j.transferId ? records[j.transferId] : undefined,
      at: j.createdAt,
    })),
    ...Object.values(records)
      .filter((r) => !known.has(r.transfer_id))
      .map((r) => ({ key: r.transfer_id, record: r, at: r.created_at })),
  ].sort((a, b) => b.at - a.at);
  for (const r of rows) if (current(r)) seen.current.add(r.key);
  const isCurrent = (r: Row) => !r.job?.retried && seen.current.has(r.key);
  const waiting = (r: Row) => (WAITING.includes(rowPhase(r)) ? 0 : 1);
  const currentRows = rows
    .filter(isCurrent)
    .sort((a, b) => waiting(a) - waiting(b) || b.at - a.at);
  // Attachments fetched to show in a task are the app’s own work, not files the
  // person moved: once done they're counted, not listed.
  const past = rows.filter((r) => !isCurrent(r)),
    folded = past.filter((r) => !r.job && r.record?.shown_in_task).length,
    listed = past.filter((r) => r.job || !r.record?.shown_in_task);
  const q = historyQuery.trim().toLowerCase(),
    shownHistory = q
      ? listed.filter((r) =>
          [
            transferName(r.job?.name ?? r.record?.name, r.record?.host_path ?? r.job?.hostPath),
            r.record?.host_path ?? r.job?.hostPath,
            r.record?.destination_folder,
          ].some((v) => v?.toLowerCase().includes(q)),
        )
      : listed;
  const name = c.mode === "text" ? c.name : (picking?.name ?? c.fileName),
    size =
      c.mode === "text"
        ? new Blob([c.text]).size
        : (picking?.size ?? blob?.size ?? c.fileSize);
  const dirError =
    c.destination.kind === "project_dir" ? relativeDir(c.destination.dir) : null;
  const destinationError =
    c.destination.kind === "project_dir"
      ? dirError ||
        (!projects[c.destination.project_id]
          ? "Choose an available project."
          : projects[c.destination.project_id]?.home
            ? "Home can’t receive files from the phone. Choose the Inbox or another project."
            : null)
      : c.destination.kind === "inbox" && noInbox
        ? `${profile.name} has no folder for received files. Choose another destination.`
        : c.destination.kind === "artifact"
          ? "Choose where to send this file."
          : null;
  useEffect(() => {
    const submitted = c.submitted;
    if (!submitted || jobs[submitted.jobId]?.phase !== "completed") return;
    const sent = composer.get();
    if (composer.completed(submitted.jobId) && sent.mode === "file") {
      // A file being chosen here meanwhile is not the one sent.
      if (holding.current?.fileId === sent.fileId) hold(null);
      if (fileInput.current) fileInput.current.value = "";
      void transferBytes
        .remove(composerFileKey(profile.profileId, sent.fileId))
        .catch((e: unknown) => setError(describeError(e, profile.name)));
    }
  }, [c.submitted, jobs, composer, profile.profileId, profile.name]);
  // A destination check answers for what Send showed when it was pressed: the
  // destination, the name and what it would send. Once any of them changes,
  // or this view closes, its answer, late or shown, is not about what Send
  // now shows. Compared by value: choosing the same folder again is not a change.
  const destinationCheck = useRef(0),
    destinationKey =
      c.destination.kind === "project_dir"
        ? `project_dir\0${c.destination.project_id}\0${c.destination.dir}`
        : c.destination.kind,
    content = c.mode === "text" ? c.text : blob;
  useEffect(() => {
    setSendError(null);
    setCheckingDestination(false);
    setCollision(null);
    return () => {
      destinationCheck.current++;
    };
  }, [destinationKey, name, content]);
  const send = (decision?: UploadCollision) => {
    const source =
      c.mode === "text"
        ? new Blob([c.text], {
            type: c.name.endsWith(".md") ? "text/markdown" : "text/plain",
          })
        : blob;
    if (!source) return;
    const id = manager.startUpload(name, source, c.destination, decision);
    composer.submitted(id);
    setCollision(null);
    revealCurrent.current = true;
    setError(null);
    setSendError(null);
  };
  const reviewDestination = async () => {
    const id = ++destinationCheck.current,
      current = () => destinationCheck.current === id;
    setCheckingDestination(true);
    setError(null);
    setSendError(null);
    try {
      const r = await session.query({
        kind: "upload_destination",
        name,
        destination: c.destination,
      });
      if (!current()) return;
      if (r.check.exists) setCollision(r.check);
      else send();
    } catch (e) {
      if (current()) setSendError(`Not sent. ${describeError(e, profile.name)}`);
    } finally {
      if (current()) setCheckingDestination(false);
    }
  };
  const pick = async (file: globalThis.File | undefined) => {
    if (!file) return;
    const fileId = uuidv7();
    setPicking(file);
    hold({ fileId, blob: file });
    try {
      await transferBytes.put(
        composerFileKey(profile.profileId, fileId),
        file,
      );
    } catch (e) {
      setError(
        describeError(e, profile.name) +
          " Keep the app open until you send this file.",
      );
    } finally {
      // Named once its bytes are kept, so other views find them; held here
      // either way, so this view can still send it. Text chosen meanwhile
      // stays what Send sends.
      chooseComposerFile(profile.profileId, {
        fileId,
        fileName: file.name,
        fileSize: file.size,
      });
      setPicking(null);
    }
  };
  const save = async (id: string, fileName: string) => {
    setSaving(id);
    setError(null);
    try {
      const b = await manager.blob(id);
      if (!b) {
        setGone((g) => new Set(g).add(id));
        throw new Error(
          "This file is no longer on this device. Download it again to save it.",
        );
      }
      const savedAs = await saveToPhone(b, fileName, platform);
      manager.saved(id, savedAs);
      announce(`Saved to ${savedAs}`);
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setSaving(null);
    }
  };
  const downloadAgain = (j: TransferJob) => {
    if (!j.source) return;
    setError(null);
    manager.retried(j.id);
    manager.startDownload(j.source, j.name);
    revealCurrent.current = true;
  };
  /** Puts a failed upload back in the form, for its destination to be
   * checked again on Send: nothing is sent twice without the person. */
  const sendAgain = async (j: TransferJob) => {
    if (!j.destination || j.destination.kind === "artifact") return;
    setError(null);
    const b = await manager.blob(j.id).catch(() => null);
    manager.retried(j.id);
    if (b) {
      composer.update({ mode: "file", destination: j.destination });
      await pick(new File([b], j.name, { type: b.type }));
    } else {
      const fileId = composer.get().fileId;
      hold(null);
      composer.update({ mode: "file", fileName: "", fileSize: 0, fileId: undefined, destination: j.destination });
      await transferBytes
        .remove(composerFileKey(profile.profileId, fileId))
        .catch(() => undefined);
      setError(`${j.name} is no longer on this device. Choose it again to send it.`);
    }
    composeRef.current?.scrollIntoView?.({ block: "start" });
  };
  const retry = (j: TransferJob, phase: string) =>
    j.retried
      ? undefined
      : j.direction === "download"
        ? j.source &&
          (phase === "failed" || phase === "cancelled" || (phase === "completed" && gone.has(j.id)))
          ? () => downloadAgain(j)
          : undefined
        : (phase === "failed" || phase === "cancelled") &&
            j.destination &&
            j.destination.kind !== "artifact"
          ? () => void sendAgain(j)
          : undefined;
  const cancelTransfer = async (target = cancel) => {
    if (!target) return;
    setCancellingId(target.id);
    setBusy(true);
    setError(null);
    try {
      if (target.local) await manager.cancel(target.id);
      else {
        const receipt = await session.command(
          { command: "cancel_transfer", transfer_id: target.id },
          { label: `Cancel ${target.name}` },
        );
        await commandResult(session, receipt);
      }
      setCancel(null);
    } catch (e) {
      setError(
        session.online
          ? describeError(e, profile.name)
          : "The computer hasn’t confirmed cancellation. Reconnect and check transfer history before trying again.",
      );
    } finally {
      setBusy(false);
      setCancellingId(null);
    }
  };
  const allowedName = c.mode !== "text" || /\.(md|txt)$/i.test(c.name);
  // Send is pinned only once there is something to send, so a person here
  // to collect a file keeps the whole screen for it.
  const composing = c.mode === "file" ? !!name : c.text !== "";
  const row = ({ key, job: j, record: r }: Row) =>
    j ? (
      <TransferRow
        computer={profile.name}
        key={key}
        job={j}
        cancelling={cancellingId === j.id}
        record={r}
        online={online}
        onPause={() => manager.pause(j.id)}
        onContinue={() => manager.continue(j.id)}
        onCancel={() => {
          const target = { id: j.id, local: true, name: j.name, checking: checking(r) };
          if (target.checking) setCancel(target);
          else void cancelTransfer(target);
        }}
        onSave={
          j.direction === "download" && j.phase === "completed" && !gone.has(j.id)
            ? () => void save(j.id, j.name)
            : undefined
        }
        onRetry={retry(j, rowPhase({ key, job: j, record: r, at: 0 }))}
        saving={saving === j.id}
      />
    ) : (
      r && (
        <TransferRow
          computer={profile.name}
          key={key}
          record={r}
          cancelling={cancellingId === r.transfer_id}
          online={online}
          onCancel={() => {
            const target = {
              id: r.transfer_id,
              local: false,
              name: transferName(r.name, r.host_path, r.shown_in_task ? "Task attachment" : undefined),
              checking: checking(r),
            };
            if (target.checking) setCancel(target);
            else void cancelTransfer(target);
          }}
        />
      )
    );
  const foldedNote = folded > 0 && (
    <p className="transfer-meta">
      {folded === 1
        ? "1 attachment loaded to show in a task isn’t listed."
        : `${folded} attachments loaded to show in tasks aren’t listed.`}
    </p>
  );
  return (
    <Screen
      bodyRef={bodyRef}
      className="transfers-screen"
      title="Transfers"
      footer={
        composing && (
          <div className="transfer-footer">
            {name && (
              <p className="transfer-meta transfer-path">
                {name} · {bytes(size)}
              </p>
            )}
            <Button
              block
              variant="primary"
              disabled={
                !online ||
                !grants ||
                !!picking ||
                checkingDestination ||
                (c.submitted?.revision === (c.revision ?? 0) &&
                  !!c.submitted &&
                  [
                    "preparing",
                    "sending",
                    "verifying",
                    "unknown",
                    "paused",
                  ].includes(jobs[c.submitted.jobId]?.phase ?? "unknown")) ||
                !!validName(name) ||
                !!destinationError ||
                !allowedName ||
                (c.mode === "file" ? !blob : !c.text) ||
                localJobs.some(
                  (j) =>
                    j.name === name &&
                    ["preparing", "sending", "verifying"].includes(j.phase),
                )
              }
              pending={checkingDestination}
              onClick={() => void reviewDestination()}
            >
              Send
            </Button>
            {!online ? (
              <p className="transfer-meta">Connect to the computer to send.</p>
            ) : sendError ? (
              <p className="transfer-meta transfer-error" role="alert">{sendError}</p>
            ) : destinationError && destinationError !== dirError && (
              <p className="transfer-meta">{destinationError}</p>
            )}
          </div>
        )
      }
    >
      <div className="transfer-stack transfer-tray">
        <ConnectionNote at={checkedAt ?? undefined} />
        {grants === false && (
          <NotAllowed what="send or receive files" permission="transfers" />
        )}
        {(error || storageError) && (
          <Notice tone="stop">{error ?? storageError}</Notice>
        )}
        {c.error ? (
          <Notice tone="stop" action={<Button size="sm" onClick={() => composer.persistNow()}>Retry saving draft</Button>}>
            {c.error}
          </Notice>
        ) : composerRecovery && <Notice tone="warn">{composerRecovery}</Notice>}
        {!storageError && storageRecovery && (
          <Notice tone="warn">{storageRecovery}</Notice>
        )}
        <div ref={composeRef} tabIndex={-1}>
          <Section title="Send a file" className="transfer-compose">
            <div className="transfer-stack">
              <Segmented
                label="File source"
                value={c.mode}
                options={[
                  { value: "file", label: "File" },
                  { value: "text", label: "Text" },
                ]}
                onChange={(mode) => composer.update({ mode })}
              />
              {c.mode === "file" ? (
                <>
                  <label className="transfer-pick">
                    <Upload aria-hidden />
                    <span>
                      {picking ? "Keeping file on this device…" : "Choose a file"}
                    </span>
                    <input
                      ref={fileInput}
                      type="file"
                      aria-label="Choose a file"
                      onChange={(e) => void pick(e.target.files?.[0])}
                      disabled={!!picking}
                    />
                  </label>
                  {name && (
                    <div>
                      <p className="transfer-path">{name}</p>
                      <p className="transfer-meta">
                        {bytes(size)}
                        {!picking && kept && !blob && " · Select the original file again"}
                      </p>
                    </div>
                  )}
                </>
              ) : (
                <>
                  <TextField
                    label="File name"
                    value={c.name}
                    onChange={(name) => composer.update({ name })}
                    error={
                      validName(c.name) ??
                      (!allowedName
                        ? "Choose .md or .txt for a text file."
                        : null)
                    }
                  />
                  <TextArea
                    label="File text"
                    value={c.text}
                    onChange={(text) => composer.update({ text })}
                    rows={4}
                    maxRows={12}
                  />
                  <p className="transfer-meta">
                    {bytes(size)} ·{" "}
                    {c.error
                      ? "Held in memory; keep the app open"
                      : "Saved on this device until you send"}
                  </p>
                </>
              )}
              <DestinationPicker
                value={c.destination}
                onChange={(destination) => composer.update({ destination })}
              />
              <p className="transfer-rule">
                If this name exists, review replace, keep both, or cancel before
                sending. Cancelling a transfer discards its staged bytes; a file
                already delivered stays in place.
              </p>
            </div>
          </Section>
        </div>
        {currentRows.length > 0 && (
          <div ref={currentRef} tabIndex={-1}>
            <Section title="Current transfers">
              <div className="transfer-stack">{currentRows.map(row)}</div>
            </Section>
          </div>
        )}
        {listed.length > 0 ? (
          <Section
            title="History"
            action={
              <Button
                variant="quiet"
                size="sm"
                aria-expanded={showHistory}
                aria-controls={historyBody}
                onClick={() => setShowHistory((v) => !v)}
              >
                {showHistory ? "Hide" : `Show ${listed.length}`}
              </Button>
            }
          >
            <div id={historyBody} className="transfer-stack" hidden={!showHistory}>
              {showHistory && (
                <>
                  {checkedAt && (
                    <p className="transfer-meta">
                      Computer history checked <Age at={checkedAt} suffix=" ago" />
                    </p>
                  )}
                  {listed.length > 5 && (
                    <TextField
                      label="Search transfers"
                      hideLabel
                      placeholder="Search transfers"
                      type="search"
                      value={historyQuery}
                      onChange={setHistoryQuery}
                      enterKeyHint="search"
                    />
                  )}
                  {shownHistory.map(row)}
                  {!shownHistory.length && (
                    <Empty title="No matching transfers">
                      Search by a file’s name or where it went.
                    </Empty>
                  )}
                  {foldedNote}
                </>
              )}
            </div>
          </Section>
        ) : folded > 0 ? (
          <Section title="History">{foldedNote}</Section>
        ) : (
          !currentRows.length && (
            <Section title="History">
              <Empty title="No transfers yet">
                Files you send and receive appear here, with their destination
                and verification result.
              </Empty>
            </Section>
          )
        )}
      </div>
      <Sheet
        className="transfer-sheet"
        open={!!collision}
        title={`${name} already exists`}
        onClose={() => setCollision(null)}
      >
        <div className="transfer-stack">
          <p>Choose what to do with the file at this destination.</p>
          <Button
            variant="primary"
            onClick={() =>
              collision &&
              send({ choice: "keep_both", name: collision.keep_both_name })
            }
          >
            Keep both · {collision?.keep_both_name}
          </Button>
          {collision?.replace_revision ? (
            <>
              <Notice tone="warn">
                Replace {name}. The old file will move to the computer’s Library
                trash and can be restored from Recently deleted.
              </Notice>
              <Button
                variant="warn"
                onClick={() =>
                  collision.replace_revision &&
                  send({
                    choice: "replace",
                    revision: collision.replace_revision,
                  })
                }
              >
                Replace {name}
              </Button>
            </>
          ) : (
            <Notice>
              Replace is unavailable here. {collision?.replace_unavailable}
            </Notice>
          )}
          <Button onClick={() => setCollision(null)}>Cancel send</Button>
        </div>
      </Sheet>
      <Sheet
        className="transfer-sheet"
        open={!!cancel}
        title={
          cancel?.checking
            ? `Clear ${cancel.name}?`
            : `Cancel ${cancel?.name ?? "transfer"}?`
        }
        onClose={() => {
          if (!busy) setCancel(null);
        }}
        footer={
          <Button
            block
            variant={cancel?.checking ? "primary" : "stop"}
            pending={busy}
            disabled={!online}
            onClick={() => void cancelTransfer()}
          >
            {cancel?.checking ? "I’ve checked — clear it" : "Cancel transfer"}
          </Button>
        }
      >
        <div className="transfer-stack">
          {cancel?.checking ? (
            <>
              <p>
                Clear this transfer once you’ve looked on the computer. Clearing
                changes nothing there: if the file arrived, it stays.
              </p>
              {cancel.checking.where && (
                <p className="transfer-path mono">{cancel.checking.where}</p>
              )}
            </>
          ) : (
            <p>
              This asks the computer to stop the transfer. If the file has
              already arrived, it stays at its destination. Removing a completed
              file must be done on the computer.
            </p>
          )}
          {error && <Notice tone="stop">{error}</Notice>}
        </div>
      </Sheet>
    </Screen>
  );
}

/** An upload the computer couldn't confirm: cancelling it only acknowledges a check. */
function checking(
  r: TransferRecord | undefined,
): { where: string | null } | null {
  if (r?.state.state !== "reconciliation_required") return null;
  return { where: r.publication ? publicationPath(r.publication) : null };
}
