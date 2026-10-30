// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from "react";
import {
  ArrowLeft,
  ArrowUpRight,
  ChevronDown,
  Circle,
  ImagePlus,
  ListOrdered,
  MapPin,
  Pencil,
  Redo2,
  Square,
  Undo2,
} from "lucide-react";
import { useApp, useHost, useLink } from "../../app/context";
import { announce } from "../../design/Announcer";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { Segmented } from "../../design/Chip";
import { TextArea, TextField } from "../../design/Field";
import { Empty, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { StateLine } from "../../design/StateLine";
import { uuidv7 } from "../../protocol/ids";
import type { Mark, MarkShape } from "../../protocol/types";
import {
  annotationStore,
  describe,
  editedSinceSent,
  heldLabels,
  markLabel,
  nextMarkIndex,
  summarize,
  useAnnotation,
  type SourceDraft,
} from "../../state/annotations";
import { useStore } from "../../state/store";
import { useImmersive } from "../../shell/focus";
import { Screen, useRoute } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { RailButton, Tool as ToolButton } from "../control/FullControl";
import { Canvas } from "./Canvas";
import { FIT, SHAPE_WORD, moveShape, type Tool } from "./canvas";
import { SourceStrip, Sources } from "./Sources";
import { deriveStill, originWords } from "./sources";
import { TargetChips } from "./TargetChips";
import { Comments } from "./Comments";
import { Speech } from "./Speech";
import { Repro } from "./Repro";
import { useMoments, Watch } from "./Watch";
import { unplacedMoments } from "./watch";
import { reproductionProblem } from "./trim";
import { annotationReviewIntent } from "../talk/Review";
import { useWarning } from "../../app/warnings";
import "./annotate.css";
const TOOLS = [
  { value: "stroke", icon: Pencil },
  { value: "rect", icon: Square },
  { value: "ellipse", icon: Circle },
  { value: "arrow", icon: ArrowUpRight },
  { value: "pin", icon: MapPin },
] as const;
const linkedComments = (count: number) =>
  `${count} linked ${count === 1 ? "comment" : "comments"}`;
/** Marking a picture is the whole screen, as a display under control is: the
 * picture on black, its tools and the comment on glass over it, and the
 * draft's lists and options one tap away in the Draft sheet. Steps keeps a
 * scrolling page. */
export default function Annotate({ route }: RouteProps<"annotate">) {
  const { profile, ui, session } = useApp(),
    store = annotationStore(profile.profileId),
    draft = useAnnotation(store, route.annotationId);
  const persistence = useStore(store.persistence, (s) => s),
    recovery = useStore(store.recovery, (s) => s),
    link = useLink(),
    { active } = useRoute();
  const projectName = useHost((s) =>
    draft ? s.projects[draft.projectId]?.name : undefined,
  );
  const [tool, setTool] = useState<Tool>("stroke"),
    [selected, setSelected] = useState<string[]>([]);
  const [deleting, setDeleting] = useState<Mark | null>(null),
    [derive, setDerive] = useState<"crop" | "redact" | null>(null),
    [busy, setBusy] = useState(false),
    [error, setError] = useState<string | null>(null);
  const warn = useWarning("mark.delete");
  const [recordingTarget, setRecordingTarget] = useState<HTMLDivElement | null>(
      null,
    ),
    [recording, setRecording] = useState(false);
  const [composing, setComposing] = useState(false),
    input = useRef<HTMLTextAreaElement>(null);
  const [details, setDetails] = useState(false),
    [adding, setAdding] = useState(false),
    [room, setRoom] = useState<HTMLDivElement | null>(null),
    [spoken, setSpoken] = useState<HTMLDivElement | null>(null),
    [blocked, setBlocked] = useState<string | null>(null);
  useEffect(() => {
    if (!blocked) return;
    const timer = setTimeout(() => setBlocked(null), 5000);
    return () => clearTimeout(timer);
  }, [blocked]);
  useEffect(() => {
    if (link.state !== "online" || session.state.get().readiness) return;
    let cancelled = false;
    void session
      .query({ kind: "readiness" })
      .then((r) => {
        if (!cancelled)
          session.state.set((s) =>
            s.readiness ? s : { ...s, readiness: r.report },
          );
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [link.state, session]);
  useMoments(store, draft);
  const full = !!draft && (draft.mode ?? "annotate") !== "steps";
  useImmersive(active && full);
  if (!draft)
    return (
      <Screen title="Annotation">
        {persistence ? <Notice tone="stop" action={<Button onClick={() => store.persistNow()}>Retry loading drawings</Button>}>
          {persistence}
        </Notice> : <Empty title="This draft isn’t on this device">
          Return to the conversation to choose another draft.
        </Empty>}
        {!persistence && recovery && <Notice tone="warn">{recovery}</Notice>}
      </Screen>
    );
  const d = draft,
    source = d.sources.find((s) => s.key === d.selectedSource) ?? d.sources[0],
    summary = summarize(d);
  const composer = d.composer ?? { text: "", targets: [], general: false };
  const marks = d.marks.filter(
    (m) => !m.deleted && m.source_key === source?.key,
  );
  const review = () => {
    ui.saveDraft(d.projectId, d.taskId, { annotationId: d.id });
    ui.push({ screen: "review", projectId: d.projectId, taskId: d.taskId,
      intent: route.intent ?? (d.taskId ? annotationReviewIntent(session.state.get().tasks[d.taskId]) : "new_task"),
      ...(route.handBack ? { handBack: route.handBack } : {}) });
  };
  const add = (shape: MarkShape) => {
    if (!source) return;
    const id = uuidv7(Date.now()),
      index = nextMarkIndex(d, heldLabels(store, ui, d.id));
    store.update(d.id, (v) => ({
      ...v,
      marks: [
        ...v.marks,
        {
          mark_id: id,
          label: markLabel(index),
          color: index % 6,
          source_key: source.key,
          shape,
          created_at: Date.now(),
          deleted: false,
        },
      ],
      composer: { text: v.composer?.text ?? "", targets: [id], general: false },
    }));
    setSelected([id]);
    announce(`Mark ${markLabel(index)} added`);
  };
  const editShape = (id: string, shape: MarkShape) =>
    store.update(d.id, (v) => ({
      ...v,
      marks: v.marks.map((m) => (m.mark_id === id ? { ...m, shape } : m)),
    }));
  const select = (ids: string[]) => {
    setSelected(ids);
    const key = d.marks.find(
      (m) => ids.includes(m.mark_id) && !m.deleted,
    )?.source_key;
    if (key) store.record(d.id, (v) => ({ ...v, selectedSource: key }));
  };
  // A mark tapped on the picture is what the next comment is about.
  const pick = (id: string | null) => {
    if (!id) return setSelected([]);
    select([id]);
    store.record(d.id, (v) => ({
      ...v,
      composer: { text: v.composer?.text ?? "", targets: [id], general: false },
    }));
  };
  const addComment = () => {
    if (
      !composer.text.trim() ||
      (!composer.general && !composer.targets.length)
    )
      return;
    const id = uuidv7(Date.now());
    store.update(d.id, (v) => ({
      ...v,
      comments: [
        ...v.comments,
        {
          comment_id: id,
          text: composer.text.trim(),
          mark_ids: composer.targets,
          origin: { kind: "typed" },
          link_origins: composer.targets.map(() => "manual"),
        },
      ],
      commentState: {
        ...v.commentState,
        [id]: composer.general ? "general" : "manual",
      },
      composer: { ...composer, text: "" },
    }));
    input.current?.focus({ preventScroll: true });
    announce("Comment added");
  };
  const boxes = marks
    .filter((m) => selected.includes(m.mark_id) && m.shape.kind === "rect")
    .flatMap((m) => (m.shape.kind === "rect" ? [m.shape.rect] : []));
  const applyDerive = async () => {
    if (!source || !derive || !boxes[0]) return;
    setBusy(true);
    setError(null);
    try {
      await deriveStill(
        store,
        d.id,
        source,
        derive === "crop"
          ? { kind: "crop", ...boxes[0] }
          : { kind: "redact", regions: boxes },
      );
      setDerive(null);
      setSelected([]);
      setTool("stroke");
      announce("New image saved. Re-mark the areas you want to discuss.");
    } catch (e) {
      setError(e instanceof Error ? e.message : "The image couldn’t be saved.");
    } finally {
      setBusy(false);
    }
  };
  const deleteMark = (mark: Mark) => {
    store.update(d.id, (v) => ({
      ...v,
      marks: v.marks.map((m) =>
        m.mark_id === mark.mark_id ? { ...m, deleted: true } : m,
      ),
    }));
    setDeleting(null);
    setSelected((ids) => ids.filter((id) => id !== mark.mark_id));
    announce(
      "Mark deleted. Linked comments are unresolved. Undo restores the mark and its links.",
    );
  };
  const askDelete = (mark: Mark) =>
    warn.ask(() => setDeleting(mark), () => deleteMark(mark));
  const affected = deleting
    ? d.comments.filter((c) => c.mark_ids.includes(deleting.mark_id))
    : [];
  const commentReason = !composer.text.trim()
    ? "Write a comment first."
    : !composer.general && !composer.targets.length
      ? "Choose a mark or General comment."
      : null;
  const hasVideo = d.sources.some((s) => s.mediaType.startsWith("video/"));
  const trimProblem = reproductionProblem(d);
  const invalidSteps = d.steps.some(
    (s) =>
      !s.text.trim() ||
      !d.sources.some(
        (src) =>
          src.key === s.source_key &&
          src.included !== false &&
          src.mediaType.startsWith("image/"),
      ),
  );
  const stepsReason = "Give every step an included image and a description.";
  // Why Review can't open yet, in the order the person can fix it.
  const reviewReason = !summary.sources
    ? "Add an image to review."
    : composer.text.trim()
      ? "Add your typed comment before review."
      : summary.unresolved > 0
        ? `Give ${summary.unresolved === 1 ? "the unresolved comment" : `the ${summary.unresolved} unresolved comments`} a mark or make ${summary.unresolved === 1 ? "it" : "them"} general.`
        : invalidSteps
          ? stepsReason
          : trimProblem;
  const disconnected = link.state !== "online" && (
    <Notice
      tone="unknown"
      title={
        link.state === "failed" && link.reason === "revoked"
          ? "This device’s access was revoked"
          : "Computer disconnected"
      }
    >
      Keep editing on this device. Nothing sends when the connection
      returns.
    </Notice>
  );
  const status = (
    <>
      {persistence ? (
        <Notice tone="stop" title="Not saved on device" action={<Button size="sm" onClick={() => store.persistNow()}>Retry saving drawings</Button>}>
          {persistence}
        </Notice>
      ) : (
        <StateLine phase="saved_on_phone" />
      )}
      {!persistence && recovery && <Notice tone="warn">{recovery}</Notice>}
      {d.sentRevision > 0 && (
        <p className="an-revision">
          Sent as revision {d.sentRevision}
          {editedSinceSent(d)
            ? ` · Revision ${d.sentRevision + 1} pending`
            : ""}
        </p>
      )}
      {d.pauseUnconfirmed && (
        <Notice tone="warn" title="Input pause not confirmed">
          The computer may still be receiving agent input. This is a frozen
          image; drawing here never controls the desktop.
        </Notice>
      )}
      {hasVideo && (
        <p className="fine">
          The agent receives {summary.sources} {summary.sources === 1 ? "frame" : "frames"} and{" "}
          {summary.steps} {summary.steps === 1 ? "step" : "steps"}, not the video.
        </p>
      )}
      {unplacedMoments(d).length > 0 && (
        <p className="fine">Taking a still for each comment made over the video.</p>
      )}
      {trimProblem && <p className="fine">{trimProblem}</p>}
      {invalidSteps && <p className="fine">{stepsReason}</p>}
    </>
  );
  const sheets = (
    <>
      <Sheet
        open={!!deleting}
        onClose={() => setDeleting(null)}
        title={`Delete mark ${deleting?.label ?? ""}?`}
        footer={
          <Button
            variant="stop-outline"
            onClick={() => {
              if (!deleting) return;
              warn.agreed();
              deleteMark(deleting);
            }}
          >
            Delete mark {deleting?.label}
          </Button>
        }
      >
        <p>
          {affected.length
            ? "These comments stay in the draft as unresolved:"
            : "This mark has no linked comments."}
        </p>
        <ul className="an-links">
          {affected.map((c) => (
            <li key={c.comment_id}>{c.text}</li>
          ))}
        </ul>
        <p className="fine">Undo restores the same mark and its links.</p>
        {warn.check}
      </Sheet>
      <Sheet
        open={!!derive}
        onClose={() => {
          if (!busy) setDerive(null);
        }}
        title={
          derive === "crop"
            ? "Crop into a new image"
            : "Redact into a new image"
        }
        footer={
          <Button
            variant="primary"
            pending={busy}
            onClick={() => void applyDerive()}
          >
            Create {derive === "crop" ? "cropped" : "redacted"} image
          </Button>
        }
      >
        <p>
          {derive === "crop"
            ? "Only the selected box appears in the new image."
            : "Selected boxes are painted solid into the saved image."}{" "}
          The original is excluded from the next send; its marks stay attached
          to it. Re-mark the new image and review any unresolved comments.
        </p>
        <p className="an-paragraph">
          Redaction cannot remove anything already captured on the computer or
          sent to an agent.
        </p>
        {error && <Notice tone="stop">{error}</Notice>}
      </Sheet>
    </>
  );
  if (!full)
    return (
      <Screen
        title="Annotate"
        subtitle={`${projectName ?? "Annotation"} · Draft kept on back`}
        className="an-screen"
        backLabel="Back and keep draft"
        footer={
          <div className="an-review">
            <p>
              {describe(summary)}
              {summary.unresolved > 0
                ? ` · ${summary.unresolved} unresolved`
                : ""}
            </p>
            <Button
              block
              variant="primary"
              disabled={!!reviewReason}
              onClick={review}
            >
              Review and send
            </Button>
          </div>
        }
      >
        <div className="an-stack">
          {status}
          {disconnected}
          <Sources draft={d} store={store} />
          <Segmented
            label="Draft mode"
            value="steps"
            options={[
              { value: "annotate", label: "Annotate" },
              { value: "steps", label: "Steps" },
            ]}
            onChange={(mode) => store.record(d.id, (v) => ({ ...v, mode }))}
          />
          <Repro
            draft={d}
            store={store}
            onMark={(key) =>
              store.record(d.id, (v) => ({
                ...v,
                selectedSource: key,
                mode: "annotate",
              }))
            }
          />
        </div>
        {sheets}
      </Screen>
    );
  const image = source?.mediaType.startsWith("image/") ? source : undefined,
    video = shownVideo(source) ? source : undefined;
  const targetWords = composer.general
    ? "Describe the change overall…"
    : composer.targets.length
      ? `What should change about ${d.marks
          .filter((m) => composer.targets.includes(m.mark_id))
          .map((m) => m.label)
          .join(", ")}?`
      : image && marks.length
        ? "Tap a mark to describe it…"
        : "Describe the change…";
  const endTyping = () => {
    setComposing(false);
    input.current?.blur();
  };
  const tryReview = () => {
    if (!reviewReason) return review();
    setBlocked(reviewReason);
    announce(reviewReason);
    // Take the person to what is in the way.
    if (!summary.sources) setAdding(true);
    else if (composer.text.trim()) input.current?.focus();
    else setDetails(true);
  };
  const others = d.sources.length;
  return (
    <Screen
      bare
      fixed
      title="Annotate"
      stepBack={composing ? endTyping : null}
      className={[
        "an-screen an-screen--full",
        composing && "is-composing",
        recording && "is-recording",
      ]
        .filter(Boolean)
        .join(" ")}
    >
      <h1 className="visually-hidden">Annotate</h1>
      <div className="an-stage">
        {image ? (
          <Canvas
            source={image}
            marks={marks}
            tool={tool}
            selected={selected}
            viewport={d.viewports?.[image.key] ?? FIT}
            clock={d.createdAt}
            room={room}
            onViewport={(view) =>
              store.record(d.id, (v) => ({
                ...v,
                viewports: { ...v.viewports, [image.key]: view },
              }))
            }
            onAdd={add}
            onMove={editShape}
            onSelect={pick}
            onDelete={(id) => {
              const mark = marks.find((m) => m.mark_id === id);
              if (mark) askDelete(mark);
            }}
          />
        ) : null}
      </div>
      <div className="an-overlay">
        <div className="an-top">
          <button
            type="button"
            className="full-tool full-tool--round"
            aria-label="Back and keep draft"
            title="Back and keep draft"
            onClick={() => ui.back()}
          >
            <ArrowLeft aria-hidden />
          </button>
          <button
            type="button"
            className="an-glass an-draft"
            aria-haspopup="dialog"
            onClick={() => setDetails(true)}
          >
            <span className="visually-hidden">Draft: </span>
            <span className="an-draft__text">
              <strong>
                {source
                  ? `${source.key}${others > 1 ? ` of ${others}` : ""} · ${originWords(source)}`
                  : "No image yet"}
              </strong>
              <span>
                {describe(summary)}
                {summary.unresolved > 0 && (
                  <span className="an-draft__warn">
                    {" "}· {summary.unresolved} unresolved
                  </span>
                )}
              </span>
            </span>
            <ChevronDown aria-hidden />
          </button>
          <RailButton />
          <button
            type="button"
            className="an-send"
            aria-label="Review and send"
            aria-disabled={reviewReason ? true : undefined}
            aria-describedby={reviewReason ? "an-review-reason" : undefined}
            onClick={tryReview}
          >
            Review
          </button>
          {reviewReason && (
            <span id="an-review-reason" className="visually-hidden">
              {reviewReason}
            </span>
          )}
        </div>
        <div className="an-notes">
          <Sources
            draft={d}
            store={store}
            strip={false}
            open={adding}
            onOpen={setAdding}
          />
          {persistence && (
            <Notice tone="stop" title="Not saved on device" action={<Button size="sm" onClick={() => store.persistNow()}>Retry saving drawings</Button>}>
              {persistence}
            </Notice>
          )}
          {!persistence && recovery && <Notice tone="warn">{recovery}</Notice>}
          {d.pauseUnconfirmed && (
            <p className="an-note">
              Input pause not confirmed. Drawing here never controls the
              desktop.
            </p>
          )}
          {link.state !== "online" && (
            <p className="an-note">
              {link.state === "failed" && link.reason === "revoked"
                ? "This device’s access was revoked."
                : "Computer disconnected."}{" "}
              Keep editing; nothing sends when it returns.
            </p>
          )}
          {blocked && (
            <p className="an-note" role="status">
              {blocked}
            </p>
          )}
          <div ref={setSpoken} className="an-notes__spoken" />
        </div>
        {/* The picture is fitted to this room; a video plays in it. */}
        <div className="an-room" ref={setRoom}>
          {video && (
            <div className="an-watch">
              <Watch draft={d} store={store} source={video} composing={composing} />
            </div>
          )}
        </div>
        {!source && (
          <div className="an-glass an-start">
            <h2>Start with an image</h2>
            <p>
              Capture the computer or add an image from this device. Then mark
              what should change.
            </p>
            <Button variant="primary" icon={ImagePlus} onClick={() => setAdding(true)}>
              Add source
            </Button>
          </div>
        )}
        {source && !image && !video && (
          <div className="an-glass an-start">
            <h2>Choose a clip moment</h2>
            <p>Open Steps to choose still frames from your clip.</p>
            <Button
              variant="primary"
              icon={ListOrdered}
              onClick={() => store.record(d.id, (v) => ({ ...v, mode: "steps" }))}
            >
              Open Steps
            </Button>
          </div>
        )}
        {image && (
          <div className="an-tools" role="group" aria-label="Mark tools">
            {TOOLS.map((t) => (
              <ToolButton
                key={t.value}
                icon={t.icon}
                label={SHAPE_WORD[t.value]}
                short={SHAPE_WORD[t.value]}
                pressed={tool === t.value}
                onClick={() => setTool(t.value)}
              />
            ))}
            <span className="an-tools__gap" aria-hidden />
            <ToolButton
              icon={Undo2}
              label="Undo"
              short="Undo"
              disabled={!store.canUndo(d.id)}
              onClick={() => store.undo(d.id)}
            />
            <ToolButton
              icon={Redo2}
              label="Redo"
              short="Redo"
              disabled={!store.canRedo(d.id)}
              onClick={() => store.redo(d.id)}
            />
          </div>
        )}
        <div className="an-bar">
          <div ref={setRecordingTarget} className="an-bar__recording" />
          {composing && (
            <TargetChips
              row
              marks={d.marks}
              targets={composer.targets}
              general={composer.general}
              onChange={(targets, general) => {
                store.record(d.id, (v) => ({
                  ...v,
                  composer: { ...composer, targets, general },
                }));
                setSelected(targets);
              }}
            />
          )}
          <div className="an-bar__row">
            <Speech
              compact
              draft={d}
              store={store}
              recordingTarget={recordingTarget}
              notesTarget={spoken}
              onRecording={setRecording}
            />
            <TextArea
              ref={input}
              label="What should change?"
              hideLabel
              className="an-bar__field"
              value={composer.text}
              maxRows={composing ? 4 : 2}
              onFocus={() => setComposing(true)}
              onChange={(text) =>
                store.record(d.id, (v) => ({
                  ...v,
                  composer: { ...composer, text },
                }))
              }
              placeholder={targetWords}
              aria-describedby={
                composing && commentReason ? "an-comment-reason" : undefined
              }
            />
          </div>
          {/* Below the field, not beside it, so the field keeps the width a
              sentence needs at any text size. */}
          {(composing || !!composer.text.trim()) && (
            <div className="an-bar__foot">
              {composing && commentReason && (
                <p id="an-comment-reason" className="fine">
                  {commentReason}
                </p>
              )}
              {composing && (
                <Button variant="secondary" size="sm" onClick={endTyping}>
                  Done typing
                </Button>
              )}
              {!!composer.text.trim() && (
                <Button
                  variant="primary"
                  size="sm"
                  disabled={!!commentReason}
                  onClick={addComment}
                >
                  Add comment
                </Button>
              )}
            </div>
          )}
        </div>
      </div>
      <Sheet
        open={details}
        onClose={() => setDetails(false)}
        size="tall"
        title="Draft"
        subtitle={`${projectName ?? "Annotation"} · Kept on this device`}
      >
        <div className="an-stack">
          {status}
          {disconnected}
          <SourceStrip
            draft={d}
            store={store}
            onPick={() => setDetails(false)}
            onAdd={() => {
              setDetails(false);
              setAdding(true);
            }}
          />
          <Button
            variant="secondary"
            icon={ListOrdered}
            onClick={() => {
              setDetails(false);
              store.record(d.id, (v) => ({ ...v, mode: "steps" }));
            }}
          >
            Steps to reproduce
          </Button>
          {image && (
            <Section title={`${image.key} · ${originWords(image)}`}>
              <div className="an-stack">
                {image.origin.kind !== "preview_capture" && (
                  <p className="fine">
                    <Age at={image.capturedAt} suffix=" ago" /> ·{" "}
                    {new Date(image.capturedAt).toLocaleTimeString()}
                  </p>
                )}
                {image.geometryChanged && (
                  <Notice tone="warn" title="Layout changed during capture">
                    This image uses the computer’s captured layout (revision{" "}
                    {image.captureGeometry?.geometry.revision}). Marks
                    describe this image only; capture again before comparing
                    with the live desktop.
                  </Notice>
                )}
                {image.origin.kind === "phone_import" && (
                  <p className="fine">
                    Imported on this device. The original capture time and
                    computer are unknown.
                  </p>
                )}
                {image.watchedAt && image.capturedAt > image.watchedAt && (
                  <Notice>
                    This capture is newer than the frame you were watching.
                  </Notice>
                )}
                <label className="an-check">
                  <input
                    type="checkbox"
                    checked={image.included !== false}
                    onChange={(e) =>
                      store.update(d.id, (v) => ({
                        ...v,
                        sources: v.sources.map((s) =>
                          s.key === image.key
                            ? { ...s, included: e.target.checked }
                            : s,
                        ),
                      }))
                    }
                  />
                  Include {image.key} in review
                </label>
                <p className="fine">
                  A mark doesn’t hide the rest of the image. Select a box
                  to crop or redact it. Existing marks stay on this
                  original.
                </p>
                <div className="an-wrap">
                  <Button
                    disabled={boxes.length !== 1}
                    onClick={() => {
                      setDetails(false);
                      setDerive("crop");
                    }}
                  >
                    Crop selected box
                  </Button>
                  <Button
                    disabled={!boxes.length}
                    onClick={() => {
                      setDetails(false);
                      setDerive("redact");
                    }}
                  >
                    Redact selected boxes
                  </Button>
                </div>
                {image.derivation && (
                  <p className="fine">
                    Derived from {image.derivedFrom}. Re-mark this image
                    to attach comments to its new pixels.
                  </p>
                )}
                {image.included === false && (
                  <Notice tone="warn">
                    Excluded from the next send. Comments linked here need
                    new targets or must be made general.
                  </Notice>
                )}
              </div>
            </Section>
          )}
          <Comments draft={d} store={store} onHighlight={select} />
          <Section
            title={`Marks · ${d.marks.filter((m) => !m.deleted).length}`}
          >
            <ul className="an-marks">
              {d.marks
                .filter((m) => !m.deleted)
                .map((m) => (
                  <li
                    className={`an-card${selected.includes(m.mark_id) ? " is-selected" : ""}`}
                    key={m.mark_id}
                  >
                    <Button
                      variant="quiet"
                      aria-pressed={selected.includes(m.mark_id)}
                      onClick={() =>
                        select(
                          selected.includes(m.mark_id)
                            ? selected.filter((id) => id !== m.mark_id)
                            : [...selected, m.mark_id],
                        )
                      }
                    >
                      {m.label} · {SHAPE_WORD[m.shape.kind]} ·{" "}
                      {m.source_key}
                    </Button>
                    <TextField
                      label={`Describe mark ${m.label}`}
                      value={d.descriptions?.[m.mark_id] ?? ""}
                      onChange={(text) =>
                        store.update(d.id, (v) => ({
                          ...v,
                          descriptions: {
                            ...v.descriptions,
                            [m.mark_id]: text,
                          },
                        }))
                      }
                    />
                    <p className="fine">
                      {linkedComments(
                        d.comments.filter((c) =>
                          c.mark_ids.includes(m.mark_id),
                        ).length,
                      )}
                    </p>
                    {selected.includes(m.mark_id) && (
                      <>
                        <ul className="an-links">
                          {d.comments
                            .filter((c) => c.mark_ids.includes(m.mark_id))
                            .map((c) => (
                              <li key={c.comment_id}>{c.text}</li>
                            ))}
                        </ul>
                        <div className="an-wrap">
                          {[
                            { label: "left", x: -0.02, y: 0 },
                            { label: "right", x: 0.02, y: 0 },
                            { label: "up", x: 0, y: -0.02 },
                            { label: "down", x: 0, y: 0.02 },
                          ].map((p) => (
                            <Button
                              key={p.label}
                              variant="quiet"
                              onClick={() =>
                                editShape(m.mark_id, moveShape(m.shape, p))
                              }
                            >
                              Move {m.label} {p.label}
                            </Button>
                          ))}
                        </div>
                      </>
                    )}
                    <Button
                      variant="quiet"
                      onClick={() => {
                        setDetails(false);
                        askDelete(m);
                      }}
                    >
                      Delete {m.label}
                    </Button>
                  </li>
                ))}
            </ul>
          </Section>
          <TextArea
            label="General note"
            value={d.generalNote ?? ""}
            onChange={(generalNote) =>
              store.update(d.id, (v) => ({ ...v, generalNote }))
            }
          />
        </div>
      </Sheet>
      {sheets}
    </Screen>
  );
}

function shownVideo(s: SourceDraft | undefined): s is SourceDraft {
  return !!s && s.origin.kind === "shown" && s.mediaType.startsWith("video/");
}
