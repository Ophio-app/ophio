// SPDX-License-Identifier: Apache-2.0
import { Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import {
  outgoingSources,
  type AnnotationDraft,
  type SourceDraft,
} from "../../state/annotations";
import type { AnnotationBundle, DeliveryState } from "../../protocol/types";
import { useSourceUrl } from "./useSourceUrl";
import { clipMoment, reproductionProblem, selectedRange } from "./trim";
import "./annotate.css";
import { ClipReview } from "./ClipReview";
import { clipSize, clipTime } from "./derivatives";
import { previewCaptureWords } from "./sources";

function ReviewedImage({
  draft,
  source,
}: {
  draft: AnnotationDraft;
  source: SourceDraft;
}) {
  const { url, error } = useSourceUrl(source);
  const moment = clipMoment(draft, source);
  const range = moment && selectedRange(draft, moment.clip);
  return (
    <figure className="an-reviewed-image">
      {url ? (
        <img src={url} alt={`Outgoing image ${source.key}`} />
      ) : (
        <Notice tone="warn">{error ?? "Opening image…"}</Notice>
      )}
      <figcaption>
        <strong>{source.key}</strong>
        {source.byteSize !== undefined && <> · {clipSize(source.byteSize)}</>}
        {moment && (
          <>
            {" "}
            · {moment.clip.key} at {(moment.atMs / 1000).toFixed(2)} s
            {range && (
              <>
                {" "}
                · {(moment.atMs / 1000 - range.start).toFixed(2)} s into trim (
                {range.start.toFixed(2)}–{range.end.toFixed(2)} s)
              </>
            )}
          </>
        )}
        {source.geometryChanged && (
          <>
            {" "}
            · Layout changed during capture; marks refer only to this captured
            image.
          </>
        )}
      </figcaption>
      {source.reproduction && (
        <details className="an-clip-identity">
          <summary>{source.key} source and fingerprint</summary>
          <dl>
            <dt>
              Original {source.reproduction.sourceKey} ·{" "}
              {clipSize(source.reproduction.sourceSize)}
            </dt>
            <dd>{source.reproduction.sourceSha256}</dd>
            <dt>Derived from range</dt>
            <dd>
              {clipTime(source.reproduction.actualRangeMs[0])}–
              {clipTime(source.reproduction.actualRangeMs[1])}
            </dd>
            <dt>Image SHA-256</dt>
            <dd>{source.sha256}</dd>
          </dl>
        </details>
      )}
    </figure>
  );
}
export function ReproductionReview({ draft }: { draft: AnnotationDraft }) {
  if (
    !draft.steps.length &&
    !draft.sources.some((s) => s.mediaType.startsWith("video/"))
  )
    return null;
  const problem = reproductionProblem(draft);
  return (
    <Section title="Reproduction review" className="an-outgoing">
      {[
        ...new Map(
          draft.sources
            .filter((s) => s.derivation?.kind === "trim")
            .map((s) => [s.derivedFrom, s]),
        ).values(),
      ].map((s) => (
        <ClipReview key={s.key} draft={draft} source={s} />
      ))}
      <h3>Frames and steps to send</h3>
      <p className="fine">
        The agent receives these images and ordered steps. The original clip and
        its audio stay on this device. Sending these steps does not send a
        video.
      </p>
      {problem && <Notice tone="warn">{problem}</Notice>}
      <div className="an-stack">
        {outgoingSources(draft).map((s) => (
          <ReviewedImage key={s.key} draft={draft} source={s} />
        ))}
      </div>
      <div className="an-reviewed-ranges">
        {[
          ...new Map(
            draft.steps.flatMap((step) => {
              if (!step.clip_range_ms) return [];
              const source = draft.sources.find(
                (s) => s.key === step.source_key,
              );
              const clip = source && clipMoment(draft, source)?.clip.key;
              return [
                [
                  `${clip ?? "Clip"}:${step.clip_range_ms.join(":")}`,
                  { clip, range: step.clip_range_ms },
                ] as const,
              ];
            }),
          ).entries(),
        ].map(([key, { clip, range }]) => (
          <p key={key} className="fine">
            {clip ?? "Clip"} range: {(range[0] / 1000).toFixed(2)}–
            {(range[1] / 1000).toFixed(2)} s
          </p>
        ))}
      </div>
      <ol className="an-outgoing-steps">
        {draft.steps.map((step, i) => {
          const source = draft.sources.find((s) => s.key === step.source_key);
          const moment = source && clipMoment(draft, source);
          return (
            <li key={i}>
              <strong>
                {step.source_key}
                {moment
                  ? ` · ${moment.clip.key} at ${(moment.atMs / 1000).toFixed(2)} s`
                  : " · Still image (no clip time)"}
              </strong>
              <p>{step.text || "Description needed"}</p>
              {(!source || source.included === false) && (
                <Notice tone="warn">
                  Choose an included image or remove this step before sending.
                </Notice>
              )}
            </li>
          );
        })}
      </ol>
      {draft.expected && (
        <p>
          <strong>Expected: </strong>
          {draft.expected}
        </p>
      )}
      {draft.actual && (
        <p>
          <strong>Actual: </strong>
          {draft.actual}
        </p>
      )}
    </Section>
  );
}
export function PreviewCaptureReceipt({ bundle }: { bundle: AnnotationBundle }) {
  return <>{bundle.sources.map(source => {
    const words = previewCaptureWords(source.origin);
    return words ? <p key={source.key} className="fine">{source.key} · {words}</p> : null;
  })}</>;
}
export function ReproductionReceipt({
  bundle,
  delivery,
}: {
  bundle: AnnotationBundle;
  delivery: DeliveryState;
}) {
  const reproduction = bundle.steps.length > 0 || bundle.sources.some(s => s.origin.kind === "clip");
  return <>
    <PreviewCaptureReceipt bundle={bundle} />
    {reproduction && <p className="fine">
      {delivery === "native_delivered"
        ? "Agent received"
        : delivery === "host_accepted" || delivery === "queued_for_boundary"
          ? "Sent to the computer:"
          : "Delivery unconfirmed for"}{" "}
      {bundle.sources.length} {bundle.sources.length === 1 ? "image" : "images"}{" "}
      and {bundle.steps.length} {bundle.steps.length === 1 ? "step" : "steps"}.
      This message contains no video or clip audio.
    </p>}
  </>;
}
