// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState, type CSSProperties, type PointerEvent, type ReactNode, type RefObject } from "react";
import {
  Eye,
  Lock,
  Unlock,
  MousePointer2,
  Scan,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import { Button, IconButton } from "../../design/Button";
import { Segmented } from "../../design/Chip";
import { Notice } from "../../design/Notice";
import { useNow } from "../../design/time";
import type {
  ControlTarget,
  InputAction,
  StreamRegion,
  TargetPoint,
} from "../../protocol/types";
import {
  clamp,
  clampPan,
  DRAG_DELAY_MS,
  followPointer,
  isDoubleTap,
  LONG_PRESS_MS,
  panWithin,
  pinchView,
  recognizeGesture,
  TAP_SLOP,
  targetPoint,
  twoFingerIntent,
  visibleRegion,
  type Point,
} from "./gestures";
import { frameFresh } from "./lease";
import type { Leaving } from "./leaving";
import type { Picture } from "./useMedia";
import { useTiltPan } from "./tilt";

/** Touch works the picture as a touchscreen; Touchpad slides the pointer. */
type PointerMode = "touch" | "touchpad";
const MODE_KEY = "control.pointer-mode";
const savedMode = (): PointerMode => {
  try {
    return localStorage.getItem(MODE_KEY) === "touchpad" ? "touchpad" : "touch";
  } catch {
    return "touch";
  }
};
interface Contact {
  start: Point;
  last: Point;
  at: number;
  distance: number;
  maxPointers: number;
  /** Touch: this finger drags with the left button held. */
  dragging: boolean;
}
/** Two fingers down together: where they began, and what they are doing once
 * one has moved far enough to say. */
interface Pair {
  gap: number;
  center: Point;
  at: number;
  intent: "zoom" | "scroll" | "tapped" | null;
}
/** What a full-screen picture's overlay draws its controls from. */
export interface PictureTools {
  /** What touches do now, as the caption says it. */
  state: string;
  /** How current the picture is, and whether that is current enough. */
  freshness: string;
  fresh: boolean;
  mode: PointerMode;
  setMode: (mode: PointerMode) => void;
  drag: boolean;
  toggleDrag: () => void;
  /** Touchpad: right-clicks now. Touch: makes the next click a right click. */
  rightClick: () => void;
  /** Touch: the next click is a right click. */
  rightNext: boolean;
  zoom: number;
  zoomIn: () => void;
  zoomOut: () => void;
  fit: () => void;
  /** Why the picture is missing, with the way to get it back. */
  notice: ReactNode;
}
const mid = (a: Point, b: Point) => ({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 });
export function LivePicture({
  target,
  canvas,
  backdrop,
  onRegion,
  picture,
  enabled = false,
  leaving = null,
  send,
  retry,
  compact = false,
  voice = false,
  full = false,
  tilt = false,
  overlay,
}: {
  compact?: boolean;
  /** Voice control holds input; the picture only watches. */
  voice?: boolean;
  /** Fills the screen: the picture keeps its shape in the middle, the whole
   * screen is the touchpad, and `overlay` draws the controls over it. */
  full?: boolean;
  /** Tipping the phone pans a zoomed picture (tilt.ts). */
  tilt?: boolean;
  overlay?: (tools: PictureTools) => ReactNode;
  target: ControlTarget;
  canvas: RefObject<HTMLCanvasElement | null>;
  /** Shown behind a zoomed picture while the part panned to arrives. */
  backdrop?: RefObject<HTMLCanvasElement | null>;
  /** Told which part of the target the view shows, to send only that part. */
  onRegion?: (region: StreamRegion | null) => void;
  picture: Picture;
  enabled?: boolean;
  leaving?: Leaving;
  send?: (action: InputAction) => boolean;
  retry: () => void;
}) {
  const now = useNow();
  const [mode, setMode] = useState<PointerMode>(savedMode);
  /** Touch: the next click is a right click, as Android's Shift is for one letter. */
  const [rightNext, setRightNext] = useState(false);
  const [locked, setLocked] = useState(true);
  const [drag, setDrag] = useState(false);
  const [view, setView] = useState({ zoom: 1, pan: { x: 0, y: 0 } });
  // Several moves can arrive before the next render.
  const viewNow = useRef(view);
  viewNow.current = view;
  const contacts = useRef(new Map<number, Contact>());
  /** Touch: the last tap, which another close by turns into a click. */
  const lastTap = useRef<{ at: number; point: Point } | null>(null);
  const pointer = useRef<TargetPoint>(picture.cursor ?? { x: 0.5, y: 0.5 });
  const [phonePointer, setPhonePointer] = useState(pointer.current);
  const held = useRef(false);
  const pair = useRef<Pair | null>(null);
  const surface = useRef<HTMLDivElement>(null);
  const size = () => {
    const box = surface.current?.getBoundingClientRect();
    return { width: box?.width ?? 0, height: box?.height ?? 0 };
  };
  // Full screen, the picture never pans off its own edges.
  const fitted = (v: { zoom: number; pan: Point }) => (full ? clampPan(v, size()) : v);
  useTiltPan(full && tilt && view.zoom > 1, (by) =>
    setView((v) => clampPan({ zoom: v.zoom, pan: { x: v.pan.x + by.x, y: v.pan.y + by.y } }, size())));
  useEffect(() => {
    const el = surface.current;
    if (!onRegion || !el) return;
    const report = () => onRegion(visibleRegion(view, el.getBoundingClientRect()));
    report();
    // Turning the phone resizes the surface under the same pan and zoom.
    const resized =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(report);
    resized?.observe(el);
    return () => resized?.disconnect();
  }, [view, onRegion]);
  useEffect(() => () => onRegion?.(null), [onRegion]);
  const fresh =
    picture.connected &&
    frameFresh(picture.at, now) &&
    picture.revision === target.geometry.revision;
  const age =
    picture.at === null
      ? null
      : Math.max(0, Math.floor((now - picture.at) / 1000));
  /** `edge`: a finger past the surface's edge counts as at the edge. */
  const atPoint = (p: Point, edge = false) => {
    const box = surface.current?.getBoundingClientRect();
    if (!box) return null;
    const v = viewNow.current;
    return targetPoint(
      edge
        ? {
            x: clamp(p.x, box.left, box.left + box.width),
            y: clamp(p.y, box.top, box.top + box.height),
          }
        : p,
      {
        left: box.left,
        top: box.top,
        width: box.width,
        height: box.height,
        zoom: v.zoom,
        pan: v.pan,
        dpr: window.devicePixelRatio,
      },
    );
  };
  const show = (next: { zoom: number; pan: Point }) => {
    viewNow.current = next;
    setView(next);
  };
  const remote = (action: InputAction) => {
    if (enabled && locked) send?.(action);
  };
  const release = () => {
    if (held.current)
      send?.({
        action: "pointer_button",
        button: "left",
        pressed: false,
        at: pointer.current,
      });
    held.current = false;
  };
  const press = (at: TargetPoint) => {
    pointer.current = at;
    remote({ action: "pointer_move", to: at });
    held.current = true;
    remote({ action: "pointer_button", button: "left", pressed: true, at });
  };
  const clickAt = (at: TargetPoint) => {
    remote({ action: "click", button: rightNext ? "right" : "left", at, count: 1 });
    setRightNext(false);
  };
  const chooseMode = (next: PointerMode) => {
    release();
    setRightNext(false);
    setMode(next);
    try {
      localStorage.setItem(MODE_KEY, next);
    } catch {
      /* Touch again next time. */
    }
  };
  /** Two fingers pinch the picture, or slide: a zoomed picture moves first,
   * under Touch as far as its edges and then the computer scrolls under the
   * fingers with the rest, scaled so the page follows them. */
  const twoFingers = (contact: Contact, other: Contact, p: Point) => {
    const box = surface.current?.getBoundingClientRect();
    const two = pair.current;
    if (!box || !two) return;
    const gap = Math.hypot(p.x - other.last.x, p.y - other.last.y);
    const before = Math.hypot(contact.last.x - other.last.x, contact.last.y - other.last.y);
    const center = mid(p, other.last), was = mid(contact.last, other.last);
    const step = { x: center.x - was.x, y: center.y - was.y };
    two.intent ??= twoFingerIntent(two.gap, gap, two.center, center);
    const v = viewNow.current;
    if (two.intent === "zoom" && before > 0)
      show(clampPan(pinchView(v, { x: was.x - box.left, y: was.y - box.top }, gap / before, step), box));
    else if (two.intent !== "scroll") return;
    else if (!enabled || !locked) show(clampPan({ zoom: v.zoom, pan: { x: v.pan.x + step.x, y: v.pan.y + step.y } }, box));
    else if (mode === "touchpad")
      remote({ action: "scroll", at: pointer.current, dx: -step.x * 4, dy: -step.y * 4 });
    else {
      const moved = panWithin(v, step, box);
      if (moved.pan.x !== v.pan.x || moved.pan.y !== v.pan.y) show({ ...v, pan: moved.pan });
      const at = atPoint(center, true);
      // Phone pixels to the computer's, so the page follows the fingers;
      // bounded by the smallest step any computer accepts.
      const scale = (target.geometry.scale || 1) * v.zoom;
      if (at && (moved.rest.x || moved.rest.y))
        remote({
          action: "scroll",
          at,
          dx: clamp((-moved.rest.x * target.geometry.width) / (box.width * scale), -1500, 1500),
          dy: clamp((-moved.rest.y * target.geometry.height) / (box.height * scale), -1500, 1500),
        });
    }
  };
  /** Touch, one finger lifted: a tap moves the pointer, a second tap clicks,
   * and a slide too quick to have begun dragging drags all at once. */
  const touchUp = (c: Contact, end: Point) => {
    if (c.maxPointers > 1) {
      lastTap.current = null;
      return;
    }
    if (Math.max(c.distance, Math.hypot(end.x - c.start.x, end.y - c.start.y)) > TAP_SLOP) {
      const from = atPoint(c.start),
        to = atPoint(end, true);
      if (from && to) {
        press(from);
        pointer.current = to;
        remote({ action: "pointer_move", to });
        release();
      }
      lastTap.current = null;
    } else {
      const at = atPoint(c.start);
      if (!at) return;
      if (isDoubleTap(lastTap.current, c.at, c.start)) clickAt(pointer.current);
      else {
        pointer.current = at;
        remote({ action: "pointer_move", to: at });
      }
      lastTap.current = { at: performance.now(), point: c.start };
    }
    setPhonePointer(pointer.current);
  };
  const down = (e: PointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    const p = { x: e.clientX, y: e.clientY };
    contacts.current.set(e.pointerId, {
      start: p,
      last: p,
      at: performance.now(),
      distance: 0,
      maxPointers: 1,
      dragging: false,
    });
    for (const c of contacts.current.values())
      c.maxPointers = Math.max(c.maxPointers, contacts.current.size);
    if (contacts.current.size > 1) release();
    if ((full || mode === "touch") && contacts.current.size === 2) {
      const [a, b] = [...contacts.current.values()].map((c) => c.last) as [Point, Point];
      pair.current = { gap: Math.hypot(a.x - b.x, a.y - b.y), center: mid(a, b), at: performance.now(), intent: null };
    }
    setPhonePointer(pointer.current);
    if (mode === "touchpad" && drag && locked && enabled && contacts.current.size === 1) {
      held.current = true;
      remote({
        action: "pointer_button",
        button: "left",
        pressed: true,
        at: pointer.current,
      });
    }
  };
  const move = (e: PointerEvent<HTMLDivElement>) => {
    const contact = contacts.current.get(e.pointerId);
    if (!contact) return;
    const p = { x: e.clientX, y: e.clientY },
      delta = { x: p.x - contact.last.x, y: p.y - contact.last.y };
    const other = [...contacts.current.entries()].find(
      ([id]) => id !== e.pointerId,
    )?.[1];
    if ((other || contact.maxPointers > 1) && (full || (mode === "touch" && enabled && locked))) {
      // Two fingers pinch or slide, whichever they start doing; the finger
      // left after either moves nothing.
      if (other) twoFingers(contact, other, p);
    } else if (!locked || !enabled) {
      if (other) {
        const previous = Math.hypot(
          contact.last.x - other.last.x,
          contact.last.y - other.last.y,
        );
        const next = Math.hypot(p.x - other.last.x, p.y - other.last.y);
        const box = surface.current?.getBoundingClientRect();
        if (box && previous > 0)
          setView((v) =>
            pinchView(
              v,
              {
                x: (contact.last.x + other.last.x) / 2 - box.left,
                y: (contact.last.y + other.last.y) / 2 - box.top,
              },
              next / previous,
              { x: delta.x / 2, y: delta.y / 2 },
            ),
          );
      } else if (view.zoom > 1)
        setView((v) => fitted({
          ...v,
          pan: { x: v.pan.x + delta.x, y: v.pan.y + delta.y },
        }));
    } else if (mode === "touch") {
      if (
        !contact.dragging &&
        Math.hypot(p.x - contact.start.x, p.y - contact.start.y) > TAP_SLOP &&
        performance.now() - contact.at >= DRAG_DELAY_MS
      ) {
        const from = atPoint(contact.start);
        if (from) {
          contact.dragging = true;
          press(from);
        }
      }
      const at = contact.dragging && held.current ? atPoint(p, true) : null;
      if (at) {
        pointer.current = at;
        remote({ action: "pointer_move", to: at });
      }
    } else if (other)
      remote({
        action: "scroll",
        at: pointer.current,
        dx: -delta.x * 2,
        dy: -delta.y * 2,
      });
    else {
      const box = surface.current?.getBoundingClientRect();
      if (box) {
        pointer.current = {
          x: clamp(pointer.current.x + delta.x / box.width / view.zoom),
          y: clamp(pointer.current.y + delta.y / box.height / view.zoom),
        };
        remote({ action: "pointer_move", to: pointer.current });
        // A zoomed picture follows the pointer toward its edges.
        if (full && enabled && locked) setView((v) => followPointer(v, pointer.current, box));
      }
    }
    contact.last = p;
    setPhonePointer(pointer.current);
    contact.distance = Math.max(
      contact.distance,
      Math.hypot(p.x - contact.start.x, p.y - contact.start.y),
    );
  };
  const up = (e: PointerEvent<HTMLDivElement>, cancelled = false) => {
    const c = contacts.current.get(e.pointerId);
    if (!c) return;
    const two = pair.current;
    if (full && two && !cancelled && two.intent === null && contacts.current.size === 2 && performance.now() - two.at < LONG_PRESS_MS && enabled && locked) {
      // Two fingers tapped together: a right click, as on a laptop's touchpad.
      two.intent = "tapped";
      const center = mid(...([...contacts.current.values()].map((x) => x.last) as [Point, Point]));
      const at = mode === "touch" ? atPoint(center) : pointer.current;
      if (at) remote({ action: "click", button: "right", at, count: 1 });
    }
    contacts.current.delete(e.pointerId);
    if (contacts.current.size === 0) pair.current = null;
    const wasHeld = held.current;
    release();
    if (wasHeld) lastTap.current = null;
    if (cancelled || wasHeld || !enabled || !locked) return;
    if (mode === "touch") touchUp(c, { x: e.clientX, y: e.clientY });
    else {
      const gesture = recognizeGesture(
        c.start,
        { x: e.clientX, y: e.clientY },
        performance.now() - c.at,
        c.maxPointers,
        c.distance,
      );
      if (gesture === "tap" || gesture === "long_press")
        remote({
          action: "click",
          button: gesture === "long_press" ? "right" : "left",
          at: pointer.current,
          count: 1,
        });
    }
  };
  const marker = enabled ? phonePointer : picture.cursor;
  const state = leaving ? (leaving === "pending" ? "Leaving control…" : "Leave unconfirmed · input off") : enabled
    ? locked
      ? mode === "touch"
        ? `Touch${rightNext ? " · next click right" : ""}`
        : `Touchpad${drag ? " · remote drag" : ""}`
      : "Move picture · input off"
    : voice
      ? "Voice control · touch off"
      : "Watch · read-only";
  const freshness = age === null
    ? "Waiting for picture"
    : fresh
      ? age < 2
        ? "Live"
        : `${age} s old`
      : !picture.connected
        ? `Picture paused · ${age} s old`
        : picture.revision !== target.geometry.revision
          ? "Display changed"
          : `Stale · ${age} s`;
  const [w, h] = picture.revision !== target.geometry.revision && picture.source
    ? [picture.source.width, picture.source.height]
    : [target.geometry.width, target.geometry.height];
  const touch = {
    onPointerDown: down,
    onPointerMove: move,
    onPointerUp: (e: PointerEvent<HTMLDivElement>) => up(e),
    onPointerCancel: (e: PointerEvent<HTMLDivElement>) => up(e, true),
    onLostPointerCapture: release,
    onContextMenu: (e: { preventDefault(): void }) => e.preventDefault(),
  };
  const errorNotice = picture.error && (
    <Notice
      tone="warn"
      action={<Button onClick={retry}>{picture.error?.includes("Display changed") ? "Reopen sharing" : "Reconnect picture"}</Button>}
    >
      {picture.error}
    </Notice>
  );
  // Zoom from the middle of what is shown, so the part in view stays there.
  const zoomTo = (next: (zoom: number) => number) => setView((v) => {
    const { width, height } = size();
    return clampPan(pinchView(v, { x: width / 2, y: height / 2 }, clamp(next(v.zoom), 1, 4) / v.zoom, { x: 0, y: 0 }), { width, height });
  });
  const content = <>
        <div
          className="picture-layer"
          style={{
            transform: `translate(${view.pan.x}px, ${view.pan.y}px) scale(${view.zoom})`,
          }}
        >
          {backdrop && <canvas ref={backdrop} aria-hidden />}
          <canvas
            ref={canvas}
            role="img"
            aria-label={`${target.label}, ${fresh ? "current picture" : "picture not current"}`}
          />
        </div>
        {marker && picture.at !== null && (
          <MousePointer2
            className="picture-pointer"
            role="img"
            aria-label={
              enabled ? "Phone pointer position" : "Computer pointer position"
            }
            size={20}
            style={{
              left: `calc(${marker.x * 100 * view.zoom}% + ${view.pan.x}px)`,
              top: `calc(${marker.y * 100 * view.zoom}% + ${view.pan.y}px)`,
            }}
          />
        )}
        {picture.at === null && (
          <span className="picture-placeholder">
            {picture.error
              ? "Picture unavailable"
              : "Connecting to the display…"}
          </span>
        )}
  </>;
  if (full)
    return (
      <div className="live-picture live-picture--full">
        <div className="picture-stage" data-scroll-x {...touch}>
          <div ref={surface} className="picture-surface" style={{ aspectRatio: `${w} / ${h}`, "--ar": w / h } as CSSProperties}>
            {content}
          </div>
        </div>
        {overlay?.({
          state, freshness, fresh, mode,
          setMode: chooseMode,
          drag,
          toggleDrag: () => { release(); setDrag((v) => !v); },
          rightClick: () => mode === "touch" ? setRightNext((v) => !v) : remote({ action: "click", button: "right", at: pointer.current, count: 1 }),
          rightNext,
          zoom: view.zoom,
          zoomIn: () => zoomTo((z) => z + 0.5),
          zoomOut: () => zoomTo((z) => z - 0.5),
          fit: () => setView({ zoom: 1, pan: { x: 0, y: 0 } }),
          notice: errorNotice,
        })}
      </div>
    );
  return (
    <div className={`live-picture${compact ? " live-picture--compact" : ""}`}>
      <div className="picture-caption">
        <span>
          <Eye size={16} aria-hidden />
          {compact ? target.label : state}
        </span>
        <span className={fresh ? "" : "picture-caption__stale"}>
          {freshness}
        </span>
      </div>
      <div
        ref={surface}
        className="picture-surface"
        data-scroll-x
        style={{ aspectRatio: `${w} / ${h}` }}
        {...touch}
      >
        {content}
        {/* On the picture, so a late frame doesn't push the controls down. The
            caption says how old it is. */}
        {!fresh && picture.at !== null && (
          <span className="picture-paused" role="status">Agent paused</span>
        )}
      </div>
      {errorNotice}
      {!compact && <div className="control-tools" aria-label="Picture controls">
        <IconButton
          icon={ZoomOut}
          label="Zoom out"
          disabled={view.zoom <= 1}
          onClick={() =>
            setView((v) => {
              const zoom = clamp(v.zoom - 0.5, 1, 4);
              // The whole picture fits at 1×; nothing may stay panned off it.
              return zoom === 1 ? { zoom, pan: { x: 0, y: 0 } } : { ...v, zoom };
            })
          }
        />
        <Button
          variant="quiet"
          icon={Scan}
          onClick={() => setView({ zoom: 1, pan: { x: 0, y: 0 } })}
        >
          Fit picture
        </Button>
        <IconButton
          icon={ZoomIn}
          label="Zoom in"
          disabled={view.zoom >= 4}
          onClick={() =>
            setView((v) => ({ ...v, zoom: clamp(v.zoom + 0.5, 1, 4) }))
          }
        />
        {enabled && (
          <Button
            icon={locked ? Lock : Unlock}
            aria-pressed={locked}
            onClick={() => {
              release();
              contacts.current.clear();
              setLocked((v) => !v);
            }}
          >
            {locked ? "View locked" : "Lock view"}
          </Button>
        )}
      </div>}
      {view.zoom > 1 && !compact && (
        <div className="control-keys" aria-label="Pan picture">
          {[
            { label: "Pan left", x: 64, y: 0 },
            { label: "Pan right", x: -64, y: 0 },
            { label: "Pan up", x: 0, y: 64 },
            { label: "Pan down", x: 0, y: -64 },
          ].map((p) => (
            <Button
              key={p.label}
              size="sm"
              onClick={() =>
                setView((v) => ({
                  ...v,
                  pan: { x: v.pan.x + p.x, y: v.pan.y + p.y },
                }))
              }
            >
              {p.label}
            </Button>
          ))}
        </div>
      )}
      {enabled && !compact && (
        <div className="control-inputs">
          <Segmented
            label="Pointer mode"
            value={mode}
            options={[
              { value: "touch", label: "Touch" },
              { value: "touchpad", label: "Touchpad" },
            ]}
            onChange={chooseMode}
          />
          <p className="fine">
            {!locked
              ? "Pinch and move the picture. Lock view to send input."
              : mode === "touchpad"
                ? "Slide to move the pointer. Tap to click. Two fingers scroll."
                : "Tap to move the pointer, tap again to click. Drag to drag. Pinch to zoom; two fingers move a zoomed picture, then scroll."}
          </p>
          <div className="control-keys">
            <Button
              disabled={!locked}
              onClick={() =>
                mode === "touch"
                  ? clickAt(pointer.current)
                  : remote({
                      action: "click",
                      button: "left",
                      at: pointer.current,
                      count: 1,
                    })
              }
            >
              Click
            </Button>
            {mode === "touch" ? (
              <Button
                disabled={!locked}
                aria-pressed={rightNext}
                onClick={() => setRightNext((v) => !v)}
              >
                Right click
              </Button>
            ) : (
              <>
                <Button
                  disabled={!locked}
                  onClick={() =>
                    remote({
                      action: "click",
                      button: "right",
                      at: pointer.current,
                      count: 1,
                    })
                  }
                >
                  Right click
                </Button>
                <Button
                  disabled={!locked}
                  aria-pressed={drag}
                  onClick={() => {
                    release();
                    setDrag((v) => !v);
                  }}
                >
                  {drag ? "End drag mode" : "Remote drag"}
                </Button>
              </>
            )}
            <Button
              disabled={!locked}
              onClick={() =>
                remote({
                  action: "scroll",
                  at: pointer.current,
                  dx: 0,
                  dy: -160,
                })
              }
            >
              Scroll up
            </Button>
            <Button
              disabled={!locked}
              onClick={() =>
                remote({
                  action: "scroll",
                  at: pointer.current,
                  dx: 0,
                  dy: 160,
                })
              }
            >
              Scroll down
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
