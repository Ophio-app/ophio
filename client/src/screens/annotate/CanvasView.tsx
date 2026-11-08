// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState, type PointerEvent } from "react";
import { Scan, Trash2 } from "lucide-react";
import type { Mark, MarkShape, StrokePoint } from "../../protocol/types";
import { draftMs, type SourceDraft } from "../../state/annotations";
import {
  FIT,
  SHAPE_WORD,
  badgeRect,
  bounds,
  clamp,
  clampView,
  fitTransform,
  hitTest,
  makeShape,
  moveShape,
  outlineDistance,
  scaleShape,
  toNormalized,
  toViewport,
  zoomAbout,
  type Point,
  type Room,
  type Tool,
  type Transform,
  type Viewport,
} from "./canvas";
import { useSourceUrl } from "./useSourceUrl";
// Screen pixels. A touch this close outside the picture still draws, clamped
// to its edge; this close to a mark's line or letter picks the mark; a finger
// that moves less than this tapped; a mark smaller than this was a slip.
const EDGE = 44,
  NEAR = 18,
  SLOP = 6,
  SMALLEST = 8,
  PIN = 7;
type Gesture =
  | { kind: "draw"; start: Point; points: StrokePoint[]; moved: boolean; hit?: Mark }
  | { kind: "grab"; start: Point; mark: Mark; moved: boolean }
  | { kind: "pan"; start: Point; viewport: Viewport; moved: boolean }
  | { kind: "pinch"; viewport: Viewport; t: Transform; center: Point; distance: number }
  | { kind: "none"; start: Point; moved: boolean };
/** The picture being marked. Every touch on it draws with the chosen tool;
 * a tap picks the mark under it and a mark's letter drags it, so there is
 * no separate select or pan tool. Two fingers zoom and pan. */
export function Canvas({
  source,
  imageUrl,
  imageLabel,
  marks,
  tool,
  selected,
  viewport,
  onViewport,
  onAdd,
  onMove,
  onSelect,
  onDelete,
  room: roomElement,
  clock,
}: {
  source: SourceDraft;
  imageUrl?: string;
  imageLabel?: string;
  marks: Mark[];
  tool: Tool;
  selected: string[];
  viewport: Viewport;
  onViewport(v: Viewport): void;
  onAdd(s: MarkShape): void;
  onMove(id: string, s: MarkShape): void;
  onSelect(id: string | null): void;
  onDelete?(id: string): void;
  /** The part of the canvas the controls leave clear, where the picture fits. */
  room?: HTMLElement | null;
  clock: number;
}) {
  const { url, error } = useSourceUrl(source, imageUrl);
  const container = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ x: 320, y: 220 });
  const [clear, setClear] = useState<Room | null>(null);
  const [ink, setInk] = useState<MarkShape | null>(null);
  const pointers = useRef(new Map<number, Point>());
  const gesture = useRef<Gesture | null>(null);
  useEffect(() => {
    const element = container.current;
    if (!element) return;
    const measure = () => {
      setSize({ x: element.clientWidth, y: element.clientHeight });
      if (!roomElement) return setClear(null);
      const c = element.getBoundingClientRect(),
        r = roomElement.getBoundingClientRect();
      setClear(
        r.width > 0 && r.height > 0
          ? { x: r.left - c.left, y: r.top - c.top, width: r.width, height: r.height }
          : null,
      );
    };
    // A resize observer reports each element's size as it starts observing.
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    if (roomElement) observer.observe(roomElement);
    return () => observer.disconnect();
  }, [roomElement]);
  useEffect(() => {
    setInk(null);
    gesture.current = null;
    pointers.current.clear();
  }, [source.key, tool]);
  const room = clear ?? { x: 0, y: 0, width: size.x, height: size.y };
  const t = fitTransform(size.x, size.y, source.width, source.height, viewport, room);
  const view = (v: Viewport) =>
    clampView(v, size.x, size.y, source.width, source.height, room);
  const zoomTo = (zoom: number, at: Point) =>
    onViewport(view(zoomAbout(viewport, t, at, at, clamp(zoom, 1, 8), size.x, size.y)));
  // The wheel zooms about the pointer. React's wheel listener is passive, so
  // the page would scroll too; this one isn't.
  const latest = useRef({ viewport, zoomTo });
  latest.current = { viewport, zoomTo };
  useEffect(() => {
    const element = container.current;
    if (!element) return;
    const wheel = (e: WheelEvent) => {
      e.preventDefault();
      const r = element.getBoundingClientRect(),
        { viewport: v, zoomTo: to } = latest.current;
      to(v.zoom * Math.exp(-e.deltaY / (e.ctrlKey ? 100 : 400)), {
        x: e.clientX - r.left,
        y: e.clientY - r.top,
      });
    };
    element.addEventListener("wheel", wheel, { passive: false });
    return () => element.removeEventListener("wheel", wheel);
  }, []);
  const local = (e: PointerEvent<SVGSVGElement>): Point => {
    const r = e.currentTarget.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  const point = (p: Point): StrokePoint => ({
    ...toNormalized(p, t),
    t: draftMs(Date.now() - clock),
  });
  const onBadge = (p: Point) =>
    [...marks].reverse().find((m) => {
      const b = badgeRect(m.shape, m.label, t);
      return p.x >= b.x - 4 && p.x <= b.x + b.width + 4 && p.y >= b.y - 4 && p.y <= b.y + b.height + 4;
    });
  // Screen pixels: normalized units differ per axis on a non-square image.
  // Unclamped, so a touch far out on the letterbox finds nothing.
  const distance = (m: Mark, p: Point) =>
    outlineDistance(
      scaleShape(m.shape, t.width, t.height),
      { x: p.x - t.x, y: p.y - t.y },
      m.shape.kind === "pin" ? PIN : 0,
    );
  const nearest = (p: Point, among = marks) =>
    among.reduce<{ mark: Mark; d: number } | null>((best, m) => {
      const d = distance(m, p);
      return d <= NEAR && (!best || d < best.d) ? { mark: m, d } : best;
    }, null)?.mark;
  /** What a tap picks: a letter, then the nearest line, then the smallest
   * box or ellipse it lands inside. */
  const tapped = (p: Point, inside: boolean) =>
    onBadge(p) ??
    nearest(p) ??
    (inside
      ? marks
          .filter(
            (m) =>
              (m.shape.kind === "rect" || m.shape.kind === "ellipse") &&
              hitTest(scaleShape(m.shape, t.width, t.height), { x: p.x - t.x, y: p.y - t.y }, 0),
          )
          .sort((a, b) => {
            const x = bounds(a.shape),
              y = bounds(b.shape);
            return x.w * x.h - y.w * y.h;
          })[0]
      : undefined);
  const start = (e: PointerEvent<SVGSVGElement>) => {
    if (!url) return;
    if (e.isPrimary) {
      pointers.current.clear();
      gesture.current = null;
      setInk(null);
    }
    e.currentTarget.setPointerCapture(e.pointerId);
    const p = local(e);
    pointers.current.set(e.pointerId, p);
    if (pointers.current.size === 2) {
      const [a, b] = [...pointers.current.values()];
      if (a && b)
        gesture.current = {
          kind: "pinch",
          viewport,
          t,
          center: { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 },
          distance: Math.hypot(a.x - b.x, a.y - b.y),
        };
      setInk(null);
      return;
    }
    if (pointers.current.size > 2) return;
    const outside = Math.max(t.x - p.x, p.x - t.x - t.width, t.y - p.y, p.y - t.y - t.height);
    if (e.button === 1 || outside > EDGE) {
      gesture.current =
        viewport.zoom > 1 ? { kind: "pan", start: p, viewport, moved: false } : { kind: "none", start: p, moved: false };
      return;
    }
    const grab =
      onBadge(p) ??
      nearest(
        p,
        marks.filter((m) => selected.includes(m.mark_id)),
      );
    if (grab) {
      gesture.current = { kind: "grab", start: p, mark: grab, moved: false };
      return;
    }
    // A touch on the letterbox clamps to the image's nearest edge, so a mark
    // begun just outside the picture is kept rather than dropped.
    const first = point(p),
      hit = tool === "pin" ? nearest(p) : undefined;
    gesture.current = { kind: "draw", start: p, points: [first], moved: false, hit };
    if (tool === "pin" && !hit) setInk(makeShape(tool, [first]));
  };
  const move = (e: PointerEvent<SVGSVGElement>) => {
    const g = gesture.current;
    if (!g || !pointers.current.has(e.pointerId)) return;
    const p = local(e);
    pointers.current.set(e.pointerId, p);
    if (g.kind === "pinch") {
      const [a, b] = [...pointers.current.values()];
      if (!a || !b) return;
      onViewport(
        view(
          zoomAbout(
            g.viewport,
            g.t,
            g.center,
            { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 },
            clamp((g.viewport.zoom * Math.hypot(a.x - b.x, a.y - b.y)) / Math.max(1, g.distance), 1, 8),
            size.x,
            size.y,
          ),
        ),
      );
      return;
    }
    if (!g.moved && Math.hypot(p.x - g.start.x, p.y - g.start.y) <= SLOP) return;
    g.moved = true;
    if (g.kind === "pan")
      onViewport(
        view({
          ...g.viewport,
          panX: g.viewport.panX + (p.x - g.start.x) / size.x,
          panY: g.viewport.panY + (p.y - g.start.y) / size.y,
        }),
      );
    else if (g.kind === "grab")
      setInk(moveShape(g.mark.shape, { x: (p.x - g.start.x) / t.width, y: (p.y - g.start.y) / t.height }));
    else if (g.kind === "draw") {
      g.points.push(point(p));
      setInk(makeShape(tool, g.points));
    }
  };
  const end = (e: PointerEvent<SVGSVGElement>, cancel = false) => {
    if (!pointers.current.delete(e.pointerId)) return;
    const g = gesture.current;
    gesture.current = null;
    setInk(null);
    if (!g || cancel || g.kind === "pinch" || g.kind === "pan") return;
    if (g.kind === "none") {
      if (!g.moved) onSelect(null);
    } else if (g.kind === "grab") {
      if (g.moved && ink) onMove(g.mark.mark_id, ink);
      onSelect(g.mark.mark_id);
    } else if (!g.moved) {
      // A tap with the pin places one, unless it lands on a mark; with any
      // other tool it picks what it lands on.
      const p = g.start,
        inside = p.x >= t.x && p.x <= t.x + t.width && p.y >= t.y && p.y <= t.y + t.height,
        hit = tool === "pin" ? g.hit : tapped(p, inside);
      if (hit) onSelect(hit.mark_id);
      else if (tool === "pin" && ink) onAdd(ink);
      else onSelect(null);
    } else if (ink) {
      const b = bounds(ink);
      if (tool === "pin" || Math.max(b.w * t.width, b.h * t.height) >= SMALLEST) onAdd(ink);
    }
  };
  const grabbed = gesture.current?.kind === "grab" ? gesture.current.mark.mark_id : null;
  const shown = marks.filter((m) => !(grabbed === m.mark_id && ink));
  const only = selected.length === 1 ? marks.find((m) => m.mark_id === selected[0]) : undefined;
  const zoomed = viewport.zoom > 1.001;
  const middle = { x: room.x + room.width / 2, y: room.y + room.height / 2 };
  const pan = (x: number, y: number) =>
    onViewport(view({ ...viewport, panX: viewport.panX + x, panY: viewport.panY + y }));
  return (
    <div className="an-canvas" ref={container} data-scroll-x>
      {url ? (
        <svg
          width="100%"
          height="100%"
          role="img"
          aria-label={imageLabel ?? `${source.key}: image with ${marks.length} ${marks.length === 1 ? "mark. Open Draft to select and describe it." : "marks. Open Draft to select and describe them."}`}
          onPointerDown={start}
          onPointerMove={move}
          onPointerUp={(e) => end(e)}
          onPointerCancel={(e) => end(e, true)}
          onLostPointerCapture={(e) => end(e, true)}
        >
          <image href={url} x={t.x} y={t.y} width={t.width} height={t.height} />
          {shown.map((m) => (
            <g key={m.mark_id} style={{ color: `var(--mark-${m.color % 6})` }}>
              <Shape shape={m.shape} transform={t} selected={selected.includes(m.mark_id)} />
              <Label shape={m.shape} label={m.label} transform={t} />
            </g>
          ))}
          {ink && (
            <g style={{ color: "var(--accent)" }}>
              <Shape shape={ink} transform={t} selected />
            </g>
          )}
        </svg>
      ) : (
        <p className="an-canvas__message" role={error ? "alert" : "status"}>
          {error ?? "Opening image…"}
        </p>
      )}
      {url && (
        // Where the controls end, never beside the mark just drawn, so the
        // next mark can start anywhere.
        <div className="an-pills" style={{ left: middle.x, top: room.y + room.height - 52 }}>
          {only && !ink && onDelete && (
            <button type="button" className="an-glass an-pill" onClick={() => onDelete(only.mark_id)}>
              <Trash2 aria-hidden />
              Delete {only.label}
            </button>
          )}
          <button
            type="button"
            className={`an-glass an-pill${zoomed ? "" : " an-quiet"}`}
            onClick={() => onViewport(FIT)}
          >
            <Scan aria-hidden />
            Fit picture <span className="num">{Math.round(viewport.zoom * 100)}%</span>
          </button>
        </div>
      )}
      {url && (
        <div className="an-keys" role="group" aria-label="Picture view">
          <button type="button" onClick={() => onAdd(makeShape(tool, [{ x: 0.25, y: 0.3, t: null }, { x: 0.6, y: 0.6, t: null }]))}>
            Add {SHAPE_WORD[tool].toLowerCase()} in center
          </button>
          <button type="button" disabled={!zoomed} onClick={() => zoomTo(viewport.zoom / 1.4, middle)}>
            Zoom out
          </button>
          <button type="button" disabled={viewport.zoom >= 8} onClick={() => zoomTo(viewport.zoom * 1.4, middle)}>
            Zoom in
          </button>
          {zoomed && (
            <>
              <button type="button" onClick={() => pan(0.15, 0)}>Pan left</button>
              <button type="button" onClick={() => pan(-0.15, 0)}>Pan right</button>
              <button type="button" onClick={() => pan(0, 0.15)}>Pan up</button>
              <button type="button" onClick={() => pan(0, -0.15)}>Pan down</button>
            </>
          )}
        </div>
      )}
    </div>
  );
}
export function Shape({
  shape: s,
  transform: t,
  selected,
  detailScale = 1,
}: {
  shape: MarkShape;
  transform: Transform;
  selected: boolean;
  detailScale?: number;
}) {
  const p = (v: Point) => toViewport(v, t);
  const line = () => {
    switch (s.kind) {
      case "stroke":
        if (s.points.length === 1 && s.points[0])
          return <circle cx={p(s.points[0]).x} cy={p(s.points[0]).y} r={1} />;
        return (
          <polyline
            points={s.points
              .map((v) => {
                const q = p(v);
                return `${q.x},${q.y}`;
              })
              .join(" ")}
          />
        );
      case "rect":
        return (
          <rect
            x={p(s.rect).x}
            y={p(s.rect).y}
            width={s.rect.w * t.width}
            height={s.rect.h * t.height}
          />
        );
      case "ellipse":
        return (
          <ellipse
            cx={p(s.rect).x + (s.rect.w * t.width) / 2}
            cy={p(s.rect).y + (s.rect.h * t.height) / 2}
            rx={(s.rect.w * t.width) / 2}
            ry={(s.rect.h * t.height) / 2}
          />
        );
      case "pin":
        return <circle cx={p(s.at).x} cy={p(s.at).y} r={7 * detailScale} />;
      case "arrow": {
        const a = p(s.from),
          b = p(s.to),
          angle = Math.atan2(b.y - a.y, b.x - a.x),
          n = 13 * detailScale;
        return (
          <path
            d={`M${a.x} ${a.y} L${b.x} ${b.y} M${b.x - n * Math.cos(angle - 0.5)} ${b.y - n * Math.sin(angle - 0.5)} L${b.x} ${b.y} L${b.x - n * Math.cos(angle + 0.5)} ${b.y - n * Math.sin(angle + 0.5)}`}
          />
        );
      }
    }
  };
  return (
    <>
      <g
        fill="none"
        stroke="var(--surface)"
        strokeWidth={(selected ? 7 : 5) * detailScale}
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        {line()}
      </g>
      <g
        fill="none"
        stroke="currentColor"
        strokeWidth={(selected ? 3.5 : 2.5) * detailScale}
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        {line()}
      </g>
    </>
  );
}
function Label({
  shape,
  label,
  transform: t,
}: {
  shape: MarkShape;
  label: string;
  transform: Transform;
}) {
  const b = badgeRect(shape, label, t);
  return (
    <g>
      <rect
        x={b.x}
        y={b.y}
        width={b.width}
        height={b.height}
        rx={5}
        fill="var(--surface)"
        stroke="currentColor"
        strokeWidth={2}
      />
      <text
        x={b.x + 6}
        y={b.y + 17}
        fill="var(--ink)"
        fontSize={15}
        fontWeight={700}
      >
        {label}
      </text>
    </g>
  );
}
