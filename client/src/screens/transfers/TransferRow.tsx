// SPDX-License-Identifier: Apache-2.0
import {
  ArrowDownToLine,
  ArrowUpFromLine,
  Check,
  CircleCheck,
  Pause,
  RotateCcw,
  X,
} from "lucide-react";
import type { TransferRecord } from "../../protocol/types";
import type { TransferJob } from "./manager";
import { Button } from "../../design/Button";
import { Age } from "../../design/Age";
import { Notice } from "../../design/Notice";
import { bytes } from "../library/model";
import "./transfers.css";
import { InlineCode } from "../../design/InlineCode";
const phaseLabel = {
  preparing: "Preparing",
  pending: "Waiting",
  sending: "Sending",
  in_progress: "Transferring",
  verifying: "Verifying bytes",
  paused: "Paused",
  completed: "Completed",
  failed: "Needs attention",
  unknown: "Outcome unknown",
  reconciliation_required: "Check on the computer",
  cancelled: "Cancelled",
};
/** Where the computer may have put an upload, in the computer's own path style. */
export function publicationPath(p: { root: string; path: string }): string {
  const sep = p.root.includes("\\") && !p.root.includes("/") ? "\\" : "/";
  return `${p.root.replace(/[\\/]+$/, "")}${sep}${p.path.replace(/^[\\/]+/, "")}`;
}
export function TransferRow({
  job,
  record,
  computer = "the computer",
  online,
  onPause,
  onContinue,
  onCancel,
  onSave,
  onRetry,
  saving = false,
  cancelling = false,
}: {
  computer?: string;
  job?: TransferJob;
  record?: TransferRecord;
  online: boolean;
  onPause?: () => void;
  onContinue?: () => void;
  onCancel?: () => void;
  onSave?: () => void;
  /** Starts it over: a download fetched again, an upload's bytes back in the form. */
  onRetry?: () => void;
  saving?: boolean;
  cancelling?: boolean;
}) {
  if (!job && !record) return null;
  const direction =
      job?.direction ??
      (record?.direction === "phone_to_host" ? "upload" : "download"),
    Icon = direction === "upload" ? ArrowUpFromLine : ArrowDownToLine;
  // The computer's record decides this one: nothing on the phone can.
  const checking = record?.state.state === "reconciliation_required";
  const phase = checking
      ? "reconciliation_required"
      : (job?.phase ?? record?.state.state ?? "unknown"),
    complete = phase === "completed",
    name = transferName(
      job?.name ?? record?.name,
      record?.host_path ?? job?.hostPath,
      record?.shown_in_task ? "Task attachment" : undefined,
    ),
    size = job?.size ?? record?.size ?? 0,
    transferred = job?.transferred ?? record?.transferred ?? 0;
  // Only locally verified downloads may say Verified. A host completion cannot prove phone integrity.
  const verified =
    complete &&
    !!(direction === "upload" ? (job?.sha256 ?? record?.sha256) : job?.sha256);
  const active = [
    "preparing",
    "sending",
    "in_progress",
    "verifying",
    "pending",
  ].includes(phase);
  const hostPath = record?.host_path ?? job?.hostPath;
  const published = record?.publication
    ? publicationPath(record.publication)
    : null;
  const cleared = phase === "cancelled" && !!published;
  // An upload its connection interrupted continues by itself.
  const status = job?.interrupted
    ? phase === "paused"
      ? online
        ? "Continues shortly"
        : "Continues when connected"
      : phase === "preparing" || phase === "sending"
        ? "Continuing where it stopped"
        : phaseLabel[phase]
    : phaseLabel[phase];
  return (
    <article className="transfer-row">
      <div className="transfer-row-head">
        <Icon aria-hidden />
        <div>
          <h3>{name}</h3>
          <p className="transfer-meta">
            {direction === "upload" ? "To computer" : "To this device"} ·{" "}
            {cleared ? "Cleared after checking" : complete && record?.shown_in_task ? "Shown in a task" : status}
          </p>
        </div>
      </div>
      {active || phase === "paused" ? (
        <>
          <p className="transfer-meta num">
            {bytes(transferred)} of {bytes(size)}
            {phase === "verifying" ? " · Checking integrity" : ""}
          </p>
          {size > 0 && (
            <progress
              aria-label={`Bytes transferred for ${name}`}
              value={transferred}
              max={size}
            />
          )}
        </>
      ) : (
        <p className="transfer-meta">
          {bytes(size)} ·{" "}
          <Age
            at={job?.updatedAt ?? record?.updated_at ?? Date.now()}
            suffix=" ago"
          />
        </p>
      )}
      {verified && (
        <p className="transfer-verified">
          <CircleCheck aria-hidden />
          Verified{" "}
          <InlineCode>SHA-256 {(job?.sha256 ?? record?.sha256)?.slice(0, 12)}</InlineCode>
        </p>
      )}
      {checking && (
        <Notice tone="unknown">
          The computer couldn’t confirm the file was put in place. It may
          already be there. Check this location on the computer, then clear the
          transfer.
        </Notice>
      )}
      {record?.destination_folder && <p className="transfer-meta">{complete ? "Received in" : "Destination"}: {record.destination_folder} on {computer}</p>}
      {hostPath ? (
        <p className="transfer-path mono">{hostPath}</p>
      ) : (
        published &&
        (checking || cleared) && (
          <p className="transfer-path mono">{published}</p>
        )
      )}
      {cleared && (
        <p className="transfer-meta">
          Clearing changed nothing on the computer. If the file arrived, it is
          still there.
        </p>
      )}
      {complete && direction === "upload" && hostPath && (
        <p className="transfer-meta">
          To remove this file, delete it on the computer.
        </p>
      )}
      {complete && job?.destination?.kind === "artifact" && (
        <p className="transfer-meta">
          Messages can’t attach files sent from here. To use this file, send it
          again to the Inbox or a project folder.
        </p>
      )}
      {job?.error && !checking && (
        <Notice tone={phase === "failed" ? "stop" : "unknown"}>
          {job.error}
        </Notice>
      )}
      {record?.state.state === "failed" && !job?.error && (
        <Notice tone="stop">{record.state.reason}</Notice>
      )}
      {job?.storageError && <Notice tone="warn">{job.storageError}</Notice>}
      {complete && direction === "download" && job?.savedAs && (
        <p className="transfer-saved">
          <CircleCheck aria-hidden />
          <span>
            Saved to <span className="transfer-path">{job.savedAs}</span>
          </span>
        </p>
      )}
      <div className="transfer-actions">
        {active && onPause && (
          <Button
            className="transfer-intervention"
            icon={Pause}
            onClick={onPause}
          >
            Pause
          </Button>
        )}
        {phase === "paused" && job?.resumable && onContinue && (
          <Button variant="primary" disabled={!online} onClick={onContinue}>
            Continue
          </Button>
        )}
        {checking && onCancel && (
          <Button icon={Check} disabled={!online} onClick={onCancel}>
            Clear after checking…
          </Button>
        )}
        {!checking && !complete && phase !== "cancelled" && onCancel && (
          <Button
            className="transfer-intervention"
            variant="stop-outline"
            pending={cancelling}
            icon={X}
            disabled={!online}
            onClick={onCancel}
          >
            {cancelling ? "Cancelling" : "Cancel"} {name}
          </Button>
        )}
        {onRetry && (
          <Button icon={RotateCcw} disabled={direction === "download" && !online} onClick={onRetry}>
            {direction === "upload" ? "Send again…" : "Download again"}
          </Button>
        )}
        {complete && direction === "download" && onSave && (
          <Button
            variant={job?.savedAs ? "secondary" : "primary"}
            pending={saving}
            onClick={onSave}
          >
            {saving
              ? "Saving…"
              : job?.savedAs
                ? "Save another copy"
                : "Save to device"}
          </Button>
        )}
      </div>
      {/* No job of this phone's: an app update went to its installer, another
          phone's download is on that phone, and neither leaves a copy here. */}
      {complete && direction === "download" && !onSave && !record?.shown_in_task && (
        <p className="transfer-meta">No copy is kept on this device.</p>
      )}
    </article>
  );
}

/** A transfer's file name. The companion names an upload after the chosen
 * file, a Library or project download after its file, and an attachment by
 * its label or "Attachment"; an id is never a name. */
export function transferName(
  name: string | undefined,
  destination?: string | null,
  fallback = "File transfer",
): string {
  const opaque =
    /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  const candidates = [name, destination?.split(/[\\/]/).filter(Boolean).at(-1)];
  return (
    candidates.find(
      (candidate) => candidate?.trim() && !opaque.test(candidate.trim()),
    ) ?? fallback
  );
}
