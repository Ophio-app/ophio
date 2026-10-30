// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { useApp, useHost, useLink, describeError } from "../../app/context";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { StateLine } from "../../design/StateLine";
import { announce } from "../../design/Announcer";
import {
  annotationStore,
  type AnnotationDraft,
  type SourceDraft,
} from "../../state/annotations";
import { clipSize, clipTime, verifiedSource } from "./derivatives";
import { sendReviewedClip } from "./reproduction";
import { useSourceUrl } from "./useSourceUrl";
import "./reproduction.css";

export function ClipReview({
  draft,
  source,
}: {
  draft: AnnotationDraft;
  source: SourceDraft;
}) {
  const app = useApp(),
    link = useLink(),
    allowed = useHost((s) => s.grants?.transfers === true);
  const { url, error: storageError } = useSourceUrl(source);
  const [busy, setBusy] = useState<"save" | "send" | null>(null),
    [error, setError] = useState<string | null>(null);
  const identity = source.reproduction;
  if (!identity) return null;
  const sent = !!source.clipTransfer?.artifactId;
  const perform = async (kind: "save" | "send") => {
    setBusy(kind);
    setError(null);
    try {
      if (kind === "send") {
        await sendReviewedClip(
          {
            platform: app.platform,
            profileId: app.profile.profileId,
            session: app.session,
          },
          annotationStore(app.profile.profileId),
          draft.id,
          source,
        );
        announce("The computer confirmed the trimmed clip.");
      } else {
        const blob = await verifiedSource(source);
        await app.platform.saveToPhone?.(
          blob,
          `reproduction-${source.key}.${source.mediaType === "video/mp4" ? "mp4" : "webm"}`,
        );
        announce("Trimmed clip saved to this device.");
      }
    } catch (error) {
      setError(describeError(error, app.profile.name));
    } finally {
      setBusy(null);
    }
  };
  const snapped = identity.actualRangeMs[0] !== identity.requestedRangeMs[0];
  return (
    <section
      className="an-clip-review an-stack"
      aria-label={`Trimmed clip ${source.key}`}
    >
      <h3>Trimmed clip · {source.key}</h3>
      <p className="an-clip-facts">
        {clipTime(source.durationMs ?? 0)} · {clipSize(source.byteSize ?? 0)} ·
        No audio
      </p>
      {url && (
        <video
          className="an-video"
          src={url}
          controls
          playsInline
          preload="auto"
          aria-label={`Review trimmed clip ${source.key}`}
        />
      )}
      {(storageError || error) && (
        <Notice tone="stop">{error ?? storageError}</Notice>
      )}
      <p className="fine">
        {snapped
          ? `Starts at ${clipTime(identity.actualRangeMs[0])}, the nearest earlier point this clip can be cut.`
          : `Starts at ${clipTime(identity.actualRangeMs[0])}.`}{" "}
        Ends at {clipTime(identity.actualRangeMs[1])} in {identity.sourceKey}.
      </p>
      {identity.actualRangeMs[1] !== identity.requestedRangeMs[1] && (
        <p className="fine">
          The end follows the last complete frame in the selected range.
        </p>
      )}
      <p className="fine">
        {identity.method === "webcodecs"
          ? "Exported at 15 frames per second, up to 1280 pixels."
          : "Original video frames retained."}{" "}
        Review the whole clip before sending.
      </p>
      <details className="an-clip-identity">
        <summary>Source and fingerprints</summary>
        <dl>
          <dt>
            Original {identity.sourceKey} · {clipSize(identity.sourceSize)}
          </dt>
          <dd>{identity.sourceSha256}</dd>
          <dt>Trimmed clip SHA-256</dt>
          <dd>{source.sha256}</dd>
          <dt>Selected range</dt>
          <dd>
            {clipTime(identity.requestedRangeMs[0])}–
            {clipTime(identity.requestedRangeMs[1])}
          </dd>
        </dl>
      </details>
      <StateLine
        phase={
          busy === "send"
            ? "sending"
            : sent
              ? "saved_on_computer"
              : source.clipTransfer
                ? "unknown"
                : "saved_on_phone"
        }
        computer={app.profile.name}
        detail={
          source.clipTransfer && !sent && busy !== "send"
            ? "The computer hasn’t confirmed this clip."
            : undefined
        }
      />
      <p className="fine">
        Send this clip to {app.profile.name} as a separate file. The agent
        receives the reviewed images and steps below; it cannot play this clip
        through this app.
      </p>
      <div className="an-wrap">
        <Button
          disabled={
            !url || !!busy || sent || link.state !== "online" || !allowed
          }
          pending={busy === "send"}
          onClick={() => void perform("send")}
        >
          {sent
            ? "Clip on computer"
            : source.clipTransfer
              ? "Retry clip transfer"
              : "Send clip to computer"}
        </Button>
        {app.platform.saveToPhone && (
          <Button
            disabled={!url || !!busy}
            pending={busy === "save"}
            onClick={() => void perform("save")}
          >
            Save clip on this device
          </Button>
        )}
      </div>
      {link.state !== "online" ? (
        <p className="fine">
          {link.state === "failed" && link.reason === "revoked"
            ? "This device’s access was revoked. Pair again to send. Your clip stays on this device."
            : "Reconnect to send. Your clip stays on this device."}
        </p>
      ) : (
        !allowed && (
          <Notice tone="warn">
            This device doesn’t have permission to transfer files.
          </Notice>
        )
      )}
    </section>
  );
}
