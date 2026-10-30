// SPDX-License-Identifier: Apache-2.0
import { Fragment, useEffect, useRef, useState } from "react";
import {
  AppWindow,
  Camera,
  ChevronDown,
  ChevronUp,
  Film,
  Globe,
  ImageOff,
  ImagePlus,
  Monitor,
  Plus,
} from "lucide-react";
import { useApp, useHost, useLink, describeError } from "../../app/context";
import { Button } from "../../design/Button";
import { ListRow } from "../../design/ListRow";
import { CapabilityNote, Notice, availabilityText } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { Age } from "../../design/Age";
import type { ControlTarget } from "../../protocol/types";
import {
  MAX_SOURCES,
  outgoingSources,
  type AnnotationDraft,
  type AnnotationStore,
  type SourceDraft,
} from "../../state/annotations";
import {
  captureSource,
  importSource,
  originWords,
  SourcesFullError,
} from "./sources";
import { useSourceUrl } from "./useSourceUrl";
export function Sources({
  draft: d,
  store,
  open: shown,
  onOpen,
  strip = true,
}: {
  draft: AnnotationDraft;
  store: AnnotationStore;
  /** Whether the Add a source sheet is open, when the screen opens it. */
  open?: boolean;
  onOpen?(open: boolean): void;
  /** The row of sources and Add source; without it, only what happened. */
  strip?: boolean;
}) {
  const { platform, profile, session, ui } = useApp(),
    targets = useHost((s) => s.targets),
    link = useLink();
  const [own, setOwn] = useState(false),
    open = shown ?? own,
    setOpen = onOpen ?? setOwn;
  const [capture, setCapture] = useState(false),
    [busy, setBusy] = useState(false),
    [capturing, setCapturing] = useState<string | null>(null),
    [error, setError] = useState<string | null>(null),
    [note, setNote] = useState<string | null>(null);
  // The person chose Screen under Attach, or said yes to an agent's request
  // for a capture: offer the screens. Images or a clip they chose there are
  // added as soon as the drawing opens.
  useEffect(() => {
    const start = ui.claimSource(d.id);
    if (start?.kind === "screen") {
      setOpen(true);
      setCapture(true);
    } else if (start?.kind === "files") files(start.files);
  }, [ui, d.id]);
  const imageInput = useRef<HTMLInputElement>(null),
    videoInput = useRef<HTMLInputElement>(null);
  const select = (key: string) =>
    store.record(d.id, (v) => ({ ...v, selectedSource: key }));
  const perform = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    setNote(null);
    try {
      await fn();
      setOpen(false);
      setCapture(false);
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setBusy(false);
      setCapturing(null);
    }
  };
  /** Adds each file chosen, in order: images while the message has room
   * for them, and says what was left out. One that fails leaves the rest. */
  const files = (chosen: File[]) => {
    if (!chosen.length) return;
    void perform(async () => {
      let first: string | null = null,
        full = 0,
        failed = 0,
        failure: unknown = null;
      for (const value of chosen) {
        const isImage = !value.type.startsWith("video/");
        if (
          isImage &&
          outgoingSources(store.get()[d.id] ?? d).length >= MAX_SOURCES
        ) {
          full++;
          continue;
        }
        try {
          const source = await add(value);
          first ??= source.key;
        } catch (e) {
          if (e instanceof SourcesFullError) full++;
          else {
            failed++;
            failure ??= e;
          }
        }
      }
      // Marking starts on the first one chosen.
      if (first && chosen.length > 1) select(first);
      if (failed === chosen.length) throw failure;
      const notes = [
        full &&
          `${full === 1 ? "1 image wasn’t" : `${full} images weren’t`} added: a message holds ${MAX_SOURCES} images. Leave some out under Image options to add others.`,
        failed &&
          `${failed === 1 ? "1 file" : `${failed} files`} couldn’t be added. ${describeError(failure, profile.name)}`,
      ].filter(Boolean);
      if (notes.length) setNote(notes.join(" "));
    });
  };
  const add = async (value: File): Promise<SourceDraft> => {
    const isVideo = value.type.startsWith("video/");
    const source = await importSource(store, d.id, {
      blob: value,
      mediaType: value.type,
      origin: { kind: "phone_import", original_name: value.name },
      withRoom: true,
    });
    if (isVideo)
      store.record(d.id, (v) => ({
        ...v,
        mode: "steps",
        selectedClip: source.key,
        sources: v.sources.map((s) =>
          s.key === source.key
            ? {
                ...s,
                origin: {
                  kind: "clip",
                  duration_ms: s.durationMs ?? 0,
                  target_id: null,
                },
              }
            : s,
        ),
      }));
    return source;
  };
  return (
    <>
      {strip && (
        <SourceStrip draft={d} store={store} onAdd={() => setOpen(true)} />
      )}
      {busy && !open && <p role="status" className={strip ? "fine" : "an-note"}>Saving source on this device…</p>}
      {error && !open && <Notice tone="stop">{error}</Notice>}
      {note && !open && <Notice tone="warn">{note}</Notice>}
      <input
        className="visually-hidden"
        type="file"
        accept="image/*"
        multiple
        ref={imageInput}
        aria-label="Image files from this device"
        onChange={(e) => {
          files([...(e.target.files ?? [])]);
          e.target.value = "";
        }}
      />
      <input
        className="visually-hidden"
        type="file"
        accept="video/*"
        ref={videoInput}
        aria-label="Clip file"
        onChange={(e) => {
          files([...(e.target.files ?? [])].slice(0, 1));
          e.target.value = "";
        }}
      />
      <Sheet
        open={open}
        onClose={() => {
          if (!busy) setOpen(false);
        }}
        title="Add a source"
      >
        <div className="an-stack">
          {error && (
            <Notice
              tone="stop"
              action={
                <Button
                  disabled={busy}
                  onClick={() => imageInput.current?.click()}
                >
                  Use screenshots instead
                </Button>
              }
            >
              {error}
            </Notice>
          )}
          {busy && (
            <p
              role="status"
              className={capturing ? "visually-hidden" : undefined}
            >
              {capturing ? "Capturing…" : "Saving source on this device…"}
            </p>
          )}
          <Button
            icon={Camera}
            trailingIcon={capture ? ChevronUp : ChevronDown}
            aria-expanded={capture}
            aria-controls="an-capture-targets"
            disabled={busy || link.state !== "online"}
            onClick={() => setCapture(!capture)}
          >
            Capture the computer
          </Button>
          {link.state !== "online" && (
            <p className="fine">
              Capture needs a connection. You can still import from this device.
            </p>
          )}
          {capture && link.state === "online" && (
            <div className="an-capture" id="an-capture-targets">
              <p className="an-capture__label" id="an-capture-label">
                Which screen?
              </p>
              {targets.length ? (
                <div
                  className="group"
                  role="group"
                  aria-labelledby="an-capture-label"
                >
                  {targets.map((t) => {
                    const can = t.capture.state === "available";
                    return (
                      <Fragment key={t.target_id}>
                        <ListRow
                          icon={t.kind === "window" ? AppWindow : t.kind === "browser" ? Globe : Monitor}
                          title={t.label}
                          subtitle={
                            can
                              ? targetWords(t)
                              : `${kindWord(t)} · ${availabilityText(t.capture).label}`
                          }
                          meta={
                            capturing === t.target_id ? (
                              <span className="an-capture__busy">
                                <span
                                  className="stateline__spinner"
                                  aria-hidden
                                />
                                Capturing…
                              </span>
                            ) : undefined
                          }
                          className={
                            capturing === t.target_id ? "is-busy" : undefined
                          }
                          ariaLabel={`Capture ${t.label}`}
                          disabled={busy || !can}
                          onClick={() => {
                            setCapturing(t.target_id);
                            void perform(() =>
                              captureSource(
                                {
                                  platform,
                                  profileId: profile.profileId,
                                  session,
                                },
                                store,
                                d.id,
                                t.target_id,
                              ),
                            );
                          }}
                        />
                        {!can && (
                          <div className="an-capture__note">
                            <CapabilityNote
                              what="Capture"
                              availability={t.capture}
                            />
                          </div>
                        )}
                      </Fragment>
                    );
                  })}
                </div>
              ) : (
                <Notice tone="unknown">
                  {profile.name} hasn't reported any screens to capture.
                </Notice>
              )}
            </div>
          )}
          <Button
            icon={ImagePlus}
            disabled={busy}
            onClick={() => imageInput.current?.click()}
          >
            Images from this device
          </Button>
          <Button
            icon={Film}
            disabled={busy}
            onClick={() => videoInput.current?.click()}
          >
            Clip
          </Button>
          <p className="fine">
            Choose several images at once, up to {MAX_SOURCES} a message.
            Selected sources stay on this device until review. Import a clip to
            trim it and choose frames for the agent.
          </p>
        </div>
      </Sheet>
    </>
  );
}
/** The draft's sources to choose from, and Add source. */
export function SourceStrip({
  draft: d,
  store,
  onAdd,
  onPick,
}: {
  draft: AnnotationDraft;
  store: AnnotationStore;
  onAdd(): void;
  onPick?(key: string): void;
}) {
  return (
    <div className="an-sources" aria-label="Sources" data-scroll-x>
      {d.sources.map((s) => (
        <Thumbnail
          key={s.key}
          source={s}
          selected={
            s.key === d.selectedSource ||
            (!d.selectedSource && s.key === d.sources[0]?.key)
          }
          onClick={() => {
            store.record(d.id, (v) => ({ ...v, selectedSource: s.key }));
            onPick?.(s.key);
          }}
        />
      ))}
      <Button icon={Plus} onClick={onAdd} className="an-add-source">
        Add source
      </Button>
    </div>
  );
}
function kindWord(t: ControlTarget): string {
  return t.kind === "window"
    ? "Window"
    : t.kind === "desktop"
      ? "Whole desktop"
      : t.kind === "browser"
        ? "Browser tab"
        : "Display";
}
function targetWords(t: ControlTarget): string {
  return `${kindWord(t)} · ${t.geometry.width}\u00a0×\u00a0${t.geometry.height}`;
}
function Thumbnail({
  source: s,
  selected,
  onClick,
}: {
  source: SourceDraft;
  selected: boolean;
  onClick(): void;
}) {
  const { url } = useSourceUrl(s);
  return (
    <button
      className={`an-source${selected ? " is-selected" : ""}`}
      aria-pressed={selected}
      onClick={onClick}
      aria-label={`Source ${s.key}: ${originWords(s)}`}
    >
      {url && s.mediaType.startsWith("image/") ? (
        <img src={url} alt="" />
      ) : (
        <>
          {s.mediaType.startsWith("video/") ? (
            <Film aria-hidden />
          ) : (
            <ImageOff aria-hidden />
          )}
        </>
      )}
      <span className="an-source__text">
        <strong>
          {s.key}
          {s.included === false && s.mediaType.startsWith("image/")
            ? " · excluded"
            : ""}
        </strong>
        <span title={originWords(s)}>{originWords(s)}</span>
        <Age at={s.capturedAt} suffix=" ago" />
      </span>
    </button>
  );
}
