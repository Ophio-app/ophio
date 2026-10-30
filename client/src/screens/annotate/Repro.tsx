// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from "react";
import { ArrowDown, ArrowUp } from "lucide-react";
import { Button, IconButton } from "../../design/Button";
import { TextArea, TextField } from "../../design/Field";
import { Notice } from "../../design/Notice";
import { Section } from "../../design/ListRow";
import type {
  AnnotationDraft,
  AnnotationStore,
  SourceDraft,
} from "../../state/annotations";
import { announce } from "../../design/Announcer";
import { prepareReproduction } from "./reproduction";
import { ClipReview } from "./ClipReview";
import "./reproduction.css";
import { extractMoment } from "./sources";
import { useSourceUrl } from "./useSourceUrl";
import {
  applyClipTrim,
  boundedMoment,
  clipRange,
  clipMoment,
  stepRange,
} from "./trim";
export function Repro({
  draft: d,
  store,
  onMark,
}: {
  draft: AnnotationDraft;
  store: AnnotationStore;
  onMark(key: string): void;
}) {
  const clips = d.sources.filter(
    (s) => s.mediaType.startsWith("video/") && s.derivation?.kind !== "trim",
  );
  const clip = clips.find((s) => s.key === d.selectedClip) ?? clips[0];
  const images = d.sources.filter(
    (s) => s.mediaType.startsWith("image/") && s.included !== false,
  );
  const updateSteps = (steps: AnnotationDraft["steps"]) =>
    store.update(d.id, (v) => ({
      ...v,
      steps: steps.map((s, i) => ({ ...s, index: i + 1 })),
    }));
  const move = (i: number, delta: number) => {
    const steps = [...d.steps],
      a = steps[i],
      b = steps[i + delta];
    if (!a || !b) return;
    steps[i] = b;
    steps[i + delta] = a;
    updateSteps(steps);
  };
  return (
    <div className="an-stack">
      <p className="muted">
        Import a clip, choose a range, then create a silent clip and key frames.
        Add steps to show what happened.
      </p>
      <p className="fine">
        Desktop recording isn’t available from this computer connection. Import
        an existing recording or use screenshots instead.
      </p>
      {clips.length > 1 && (
        <label className="field">
          <span className="field__label">Clip</span>
          <select
            className="field__input"
            value={clip?.key}
            onChange={(e) =>
              store.record(d.id, (v) => ({
                ...v,
                selectedClip: e.target.value,
              }))
            }
          >
            {clips.map((c) => (
              <option key={c.key} value={c.key}>
                {c.key}
              </option>
            ))}
          </select>
        </label>
      )}
      {clip && <Clip key={clip.key} source={clip} draft={d} store={store} />}
      <Section title={`Steps · ${d.steps.length}`}>
        <ol className="an-steps">
          {d.steps.map((step, i) => (
            <li key={`${step.source_key}-${i}`} className="an-card">
              <p className="an-step-heading">Step {i + 1}</p>
              <label className="field">
                <span className="field__label">Image for step {i + 1}</span>
                <select
                  className="field__input"
                  value={step.source_key}
                  onChange={(e) =>
                    updateSteps(
                      d.steps.map((s, j) =>
                        j === i
                          ? {
                              ...s,
                              source_key: e.target.value,
                              clip_range_ms: stepRange(d, e.target.value),
                            }
                          : s,
                      ),
                    )
                  }
                >
                  {!images.some((s) => s.key === step.source_key) && (
                    <option value={step.source_key}>
                      {step.source_key} · excluded
                    </option>
                  )}
                  {images.map((s) => (
                    <option key={s.key} value={s.key}>
                      {s.key}
                    </option>
                  ))}
                </select>
              </label>
              <TextArea
                label={`Describe step ${i + 1}`}
                value={step.text}
                onChange={(text) =>
                  updateSteps(
                    d.steps.map((s, j) => (j === i ? { ...s, text } : s)),
                  )
                }
              />
              {(() => {
                const src = d.sources.find((s) => s.key === step.source_key);
                const moment = src && clipMoment(d, src);
                return moment ? (
                  <p className="fine">
                    {moment.clip.key} · frame at{" "}
                    {(moment.atMs / 1000).toFixed(2)} s
                  </p>
                ) : null;
              })()}
              {!images.some((s) => s.key === step.source_key) && (
                <Notice tone="warn">
                  This step’s image is excluded. Choose an included image or
                  remove the step.
                </Notice>
              )}
              {step.clip_range_ms && (
                <p className="fine">
                  Clip range: {(step.clip_range_ms[0] / 1000).toFixed(2)}–
                  {(step.clip_range_ms[1] / 1000).toFixed(2)} s · only the still
                  frame is sent
                </p>
              )}
              <div className="an-wrap">
                <IconButton
                  icon={ArrowUp}
                  label={`Move step ${i + 1} up`}
                  disabled={i === 0}
                  onClick={() => move(i, -1)}
                />
                <IconButton
                  icon={ArrowDown}
                  label={`Move step ${i + 1} down`}
                  disabled={i === d.steps.length - 1}
                  onClick={() => move(i, 1)}
                />
                <Button variant="quiet" onClick={() => onMark(step.source_key)}>
                  Mark {step.source_key}
                </Button>
                <Button
                  variant="quiet"
                  onClick={() => updateSteps(d.steps.filter((_, j) => j !== i))}
                >
                  Remove step {i + 1}
                </Button>
              </div>
            </li>
          ))}
        </ol>
        <Button
          disabled={!images.length}
          onClick={() => {
            const s =
              images.find((s) => s.key === d.selectedSource) ?? images[0];
            if (s)
              updateSteps([
                ...d.steps,
                {
                  index: d.steps.length + 1,
                  source_key: s.key,
                  text: "",
                  clip_range_ms: stepRange(d, s.key),
                },
              ]);
          }}
        >
          Add step from image
        </Button>
        {!images.length && (
          <p className="fine">Add an image or a clip moment first.</p>
        )}
      </Section>
      <TextArea
        label="Expected"
        value={d.expected ?? ""}
        onChange={(expected) => store.update(d.id, (v) => ({ ...v, expected }))}
      />
      <TextArea
        label="Actual"
        value={d.actual ?? ""}
        onChange={(actual) => store.update(d.id, (v) => ({ ...v, actual }))}
      />
    </div>
  );
}
function Clip({
  source,
  draft: d,
  store,
}: {
  source: SourceDraft;
  draft: AnnotationDraft;
  store: AnnotationStore;
}) {
  const { url, error: sourceError } = useSourceUrl(source),
    video = useRef<HTMLVideoElement>(null);
  const exportJob = useRef<AbortController | null>(null);
  const [exporting, setExporting] = useState(false);
  useEffect(() => () => exportJob.current?.abort(), []);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState<string | null>(null);
  const edits = d.clipEdits?.[source.key] ?? {
    at: 0,
    start: "0",
    end: String((source.durationMs ?? 0) / 1000),
  };
  const { at, start, end } = edits;
  const edit = (patch: Partial<typeof edits>) =>
    store.record(d.id, (v) => ({
      ...v,
      clipEdits: { ...v.clipEdits, [source.key]: { ...edits, ...patch } },
    }));
  const setAt = (at: number) => edit({ at });
  const setStart = (start: string) => edit({ start });
  const setEnd = (end: string) => edit({ end });
  const duration = (source.durationMs ?? 0) / 1000;
  const range = clipRange(start, end, duration);
  useEffect(() => {
    const selected = clipRange(start, end, duration);
    if (!selected) return;
    const time = boundedMoment(at, selected);
    if (video.current && Math.abs(video.current.currentTime - time) > 0.0005)
      video.current.currentTime = time;
    if (time !== at)
      store.record(d.id, (v) => ({
        ...v,
        clipEdits: { ...v.clipEdits, [source.key]: { start, end, at: time } },
      }));
  }, [start, end, at, duration, store, d.id, source.key]);
  const seek = (seconds: number) => {
    const v = video.current;
    if (!v) return;
    if (!range) return;
    const time = boundedMoment(seconds, range);
    v.currentTime = time;
    setAt(time);
  };
  const makeClip = async () => {
    if (!range || exportJob.current) return;
    const controller = new AbortController();
    exportJob.current = controller;
    setExporting(true);
    setError(null);
    announce("Creating a clip on this device.");
    try {
      await prepareReproduction(store, d.id, source, range, controller.signal);
      announce(
        "Clip and three key frames saved on this device. Review them before sending.",
      );
    } catch (e) {
      setError(
        controller.signal.aborted
          ? "Clip creation cancelled. Your draft is kept."
          : e instanceof Error
            ? e.message
            : String(e),
      );
    } finally {
      exportJob.current = null;
      setExporting(false);
    }
  };
  const derived = d.sources.filter(
    (s) => s.derivedFrom === source.key && s.derivation?.kind === "trim",
  );
  const latest = derived.at(-1);
  const add = async () => {
    if (!video.current) return;
    if (!range) {
      setError("Choose a range within the clip, with its end after its start.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const image = await extractMoment(
        store,
        d.id,
        source,
        video.current,
        range,
      );
      store.update(d.id, (v) => ({
        ...v,
        steps: [
          ...v.steps,
          {
            index: v.steps.length + 1,
            source_key: image.key,
            text: "",
            clip_range_ms: [
              Math.round(Number(start) * 1000),
              Math.round(Number(end) * 1000),
            ],
          },
        ],
      }));
    } catch (e) {
      setError(
        e instanceof Error ? e.message : "This moment couldn’t be saved.",
      );
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="an-card an-stack">
      {url && (
        <video
          ref={video}
          src={url}
          preload="auto"
          onLoadedMetadata={(e) => {
            e.currentTarget.currentTime = range ? boundedMoment(at, range) : 0;
          }}
          playsInline
          muted
          className="an-video"
          onError={() =>
            setError("This clip can’t be played. Import screenshots instead.")
          }
          onTimeUpdate={(e) => {
            if (range) setAt(boundedMoment(e.currentTarget.currentTime, range));
          }}
        />
      )}
      {(error || sourceError) && (
        <Notice tone="warn">{error ?? sourceError}</Notice>
      )}
      <label className="field">
        <span className="field__label">
          Clip position · {at.toFixed(2)} s / {duration.toFixed(2)} s
        </span>
        <input
          aria-label="Clip position"
          type="range"
          min={range?.start ?? 0}
          max={range?.end ?? duration}
          disabled={!range}
          step={0.001}
          value={at}
          onChange={(e) => seek(Number(e.target.value))}
        />
      </label>
      <div className="an-wrap">
        <Button disabled={!range} onClick={() => seek(at - 1)}>
          −1 s
        </Button>
        <Button disabled={!range} onClick={() => seek(at - 1 / 30)}>
          −1 frame
        </Button>
        <Button disabled={!range} onClick={() => seek(at + 1 / 30)}>
          +1 frame
        </Button>
        <Button disabled={!range} onClick={() => seek(at + 1)}>
          +1 s
        </Button>
      </div>
      <p className="fine">
        Frame buttons step 1/30 second. The browser doesn’t report this clip’s
        frame rate.
      </p>
      <details open className="an-repro-range">
        <summary>Clip range</summary>
        <div className="an-stack">
          <TextField
            label="Range start (seconds)"
            type="number"
            disabled={exporting}
            value={start}
            onChange={setStart}
            min={0}
            max={duration}
            step={0.01}
          />
          <TextField
            label="Range end (seconds)"
            type="number"
            disabled={exporting}
            value={end}
            onChange={setEnd}
            min={0}
            max={duration}
            step={0.01}
          />
          <p className="fine">
            Up to 60 seconds. The exported clip has no audio. Android may move
            the start to an earlier keyframe; the review shows the actual cut.
          </p>
          {!range && (
            <Notice tone="warn">
              Choose a start and end inside the clip, with the end after the
              start.
            </Notice>
          )}
          <Button
            variant="primary"
            disabled={!range || busy || exporting}
            onClick={() => void makeClip()}
          >
            Create clip and key frames
          </Button>
          <Button
            disabled={!range || exporting}
            onClick={() => {
              if (range)
                store.update(d.id, (v) => applyClipTrim(v, source.key, range));
            }}
          >
            Keep frames in this range
          </Button>
          {exporting && (
            <div className="an-clip-busy" role="status">
              <p>Creating clip on this device…</p>
              <Button onClick={() => exportJob.current?.abort()}>
                Cancel clip creation
              </Button>
            </div>
          )}
          {error && (
            <Button
              onClick={() =>
                document
                  .querySelector<HTMLInputElement>(
                    'input[aria-label="Image files from this device"]',
                  )
                  ?.click()
              }
            >
              Use screenshots instead
            </Button>
          )}
          <p className="fine">
            Creating a clip sends nothing. Earlier frames, marks and steps stay
            in your draft; frames outside the range are excluded.
          </p>
        </div>
      </details>
      <Button
        pending={busy}
        disabled={!url || !range || exporting}
        onClick={() => void add()}
      >
        {busy ? "Saving moment…" : "Add this moment"}
      </Button>
      {latest && <ClipReview draft={d} source={latest} />}
    </div>
  );
}
