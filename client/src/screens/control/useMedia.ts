// SPDX-License-Identifier: Apache-2.0
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import { useApp, useLink } from "../../app/context";
import { parseTileFrame, type SourceRegion } from "../../protocol/binary";
import type {
  ControlTarget,
  StreamParams,
  StreamRegion,
} from "../../protocol/types";
import { useKeepAwake } from "../../platform/awake";
import { useNow } from "../../design/time";
import { frameFresh } from "./lease";
import { TileModel } from "./tiles";

/** Zoomed in or not, the picture keeps to the same size limit. */
const PARAMS: StreamParams = {
  max_width: 1600,
  max_height: 1200,
  quality: 78,
  max_fps: 12,
  window: 2,
};
/** Ask for a zoomed view's region once it has been still this long. */
const REGION_SETTLE_MS = 180;
const sameRegion = (a: StreamRegion | null, b: StreamRegion | null) =>
  a === b ||
  (!!a &&
    !!b &&
    a.x === b.x &&
    a.y === b.y &&
    a.width === b.width &&
    a.height === b.height);
const withRegion = (region: StreamRegion | null): StreamParams =>
  region ? { ...PARAMS, region } : PARAMS;

/** A canvas ref that moves what its element shows, and where the element is
 * placed, to a new element when a layout swaps it (typing shows a smaller
 * picture). A new element starts blank and later frames paint only what
 * changed. */
function carried(): RefObject<HTMLCanvasElement | null> {
  let element: HTMLCanvasElement | null = null;
  let shown: HTMLCanvasElement | null = null;
  return {
    get current() {
      return element;
    },
    set current(next: HTMLCanvasElement | null) {
      if (next && shown && next !== shown && shown.width && shown.height) {
        next.width = shown.width;
        next.height = shown.height;
        next.getContext("2d")?.drawImage(shown, 0, 0);
        for (const edge of ["left", "top", "width", "height"] as const)
          next.style[edge] = shown.style[edge];
      }
      if (next) shown = next;
      element = next;
    },
  } as RefObject<HTMLCanvasElement | null>;
}

export interface Picture {
  at: number | null;
  revision: number | null;
  error: string | null;
  connected: boolean;
  /** Where the computer's pointer is, in fractions of the whole target. */
  cursor?: { x: number; y: number } | null;
  /** The target's size in pixels, from the latest frame. */
  source?: { width: number; height: number } | null;
}
const initial: Picture = {
  at: null,
  revision: null,
  error: null,
  connected: false,
};

export function useMedia(target: ControlTarget | undefined, visible: boolean) {
  const { platform, profile } = useApp();
  const online = useLink().state === "online";
  const latest = useRef<Picture>(initial);
  const canvas = useMemo(carried, []);
  // What the canvas last showed, kept behind a region so a pan never uncovers
  // an empty surface while the newly shown part is on its way.
  const backdrop = useMemo(carried, []);
  const wanted = useRef<StreamRegion | null>(null);
  const adjust = useRef<(() => void) | null>(null);
  const settle = useRef<ReturnType<typeof setTimeout>>(undefined);
  const [picture, setPicture] = useState<Picture>(initial);
  const reopenRequested = useRef(false);
  const [reopening, setReopening] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const id = target?.target_id;
  const capture = target?.capture.state;
  const captureAllowed = capture === "available" || reopening;
  const now = useNow();
  useKeepAwake(
    platform,
    visible && online && picture.connected && frameFresh(picture.at, now),
  );

  useEffect(() => {
    const update = (patch: Partial<Picture>) => {
      latest.current = { ...latest.current, ...patch };
      setPicture(latest.current);
    };
    update({ connected: false });
    if (!id || !visible || !online || !captureAllowed) return;
    update({ at: null, revision: null, error: null });
    const link = platform.media(profile.profileId);
    const model = new TileModel();
    let disposed = false,
      busy = false,
      epoch = 0,
      opened = false,
      sent = false;
    let geometryRevision: number | undefined;
    let pending: { data: Uint8Array; at: number } | null = null;
    // The region asked of the computer, and the one the canvas shows.
    let asked: StreamRegion | null = null;
    let shown: SourceRegion | null = null;
    const place = (
      el: HTMLCanvasElement,
      region: SourceRegion | null,
      source: { width: number; height: number },
    ) => {
      const s = el.style;
      s.left = region ? `${(region.x / source.width) * 100}%` : "";
      s.top = region ? `${(region.y / source.height) * 100}%` : "";
      s.width = region ? `${(region.width / source.width) * 100}%` : "";
      s.height = region ? `${(region.height / source.height) * 100}%` : "";
    };
    const keepBehind = (
      el: HTMLCanvasElement,
      source: { width: number; height: number },
    ) => {
      const back = backdrop.current;
      const ctx = back?.getContext("2d");
      if (!back || !ctx || !el.width || !el.height) return;
      const scale = Math.min(
        1,
        PARAMS.max_width / source.width,
        PARAMS.max_height / source.height,
      );
      const w = Math.max(1, Math.round(source.width * scale)),
        h = Math.max(1, Math.round(source.height * scale));
      if (back.width !== w || back.height !== h) {
        back.width = w;
        back.height = h;
      }
      const area = shown ?? { x: 0, y: 0, ...source };
      ctx.drawImage(
        el,
        area.x * scale,
        area.y * scale,
        area.width * scale,
        area.height * scale,
      );
    };
    const clear = () => {
      epoch++;
      model.reset();
      pending = null;
      shown = null;
      const el = canvas.current;
      el?.getContext("2d")?.clearRect(0, 0, el.width, el.height);
      if (el) place(el, null, { width: 1, height: 1 });
      const back = backdrop.current;
      back?.getContext("2d")?.clearRect(0, 0, back.width, back.height);
      update({ at: null, revision: null, cursor: null, source: null });
    };
    let reopen = reopenRequested.current;
    reopenRequested.current = false;
    const open = () => {
      if (sent || disposed) return;
      sent = link.send({
        type: "open",
        target_id: id,
        params: { ...withRegion(wanted.current), reopen },
      });
      if (sent) {
        reopen = false;
        asked = wanted.current;
      }
    };
    adjust.current = () => {
      if (disposed || !opened || sameRegion(asked, wanted.current)) return;
      asked = wanted.current;
      link.send({ type: "adjust", params: withRegion(asked) });
    };
    const recover = () => {
      model.reset();
      link.send({ type: "request_keyframe" });
    };
    const draw = async (data: Uint8Array, at: number) => {
      busy = true;
      const generation = epoch;
      let seq: number | null = null;
      const decoded: ImageBitmap[] = [];
      try {
        const { header, tiles } = parseTileFrame(data);
        seq = header.seq;
        if (!opened || header.geometryRevision !== geometryRevision) {
          recover();
          return;
        }
        const rects = model.accept(header, tiles);
        if (!rects) {
          if (!model.ready) recover();
          return;
        }
        // Independent JPEG decoding does not occupy the input/command path.
        const results = await Promise.allSettled(
          tiles.map(async (t) => {
            const bitmap = await createImageBitmap(
              new Blob([t.jpeg.slice().buffer], { type: "image/jpeg" }),
            );
            decoded.push(bitmap);
            return bitmap;
          }),
        );
        if (disposed || generation !== epoch) return;
        const el = canvas.current;
        const ctx = el?.getContext("2d");
        if (!el || !ctx)
          throw new Error("This device could not display the picture.");
        const bitmaps = results.map((r) => {
          if (r.status === "rejected")
            throw new Error("A picture tile could not be decoded.");
          return r.value;
        });
        for (let i = 0; i < bitmaps.length; i++) {
          const rect = rects[i],
            bitmap = bitmaps[i];
          if (
            !rect ||
            !bitmap ||
            bitmap.width !== rect.width ||
            bitmap.height !== rect.height
          )
            throw new Error("The picture tile dimensions changed.");
        }
        const source = { width: header.srcWidth, height: header.srcHeight };
        if (header.keyframe) {
          const region = header.region;
          if (
            latest.current.at !== null &&
            JSON.stringify(region) !== JSON.stringify(shown)
          )
            keepBehind(el, source);
          el.width = header.encWidth;
          el.height = header.encHeight;
          place(el, region, source);
          shown = region;
        }
        for (let i = 0; i < bitmaps.length; i++) {
          const rect = rects[i],
            bitmap = bitmaps[i];
          if (rect && bitmap) ctx.drawImage(bitmap, rect.x, rect.y);
        }
        const area = header.region ?? { x: 0, y: 0, ...source };
        update({
          at,
          revision: header.geometryRevision,
          connected: true,
          error: null,
          cursor: header.cursor
            ? {
                x:
                  (area.x + (header.cursor.x / header.encWidth) * area.width) /
                  source.width,
                y:
                  (area.y +
                    (header.cursor.y / header.encHeight) * area.height) /
                  source.height,
              }
            : null,
          source:
            latest.current.source?.width === source.width &&
            latest.current.source.height === source.height
              ? latest.current.source
              : source,
        });
      } catch (e) {
        if (!disposed) {
          update({
            connected: false,
            error:
              e instanceof Error ? e.message : "The picture could not be read.",
          });
          recover();
        }
      } finally {
        decoded.forEach((b) => b.close());
        if (seq !== null && !disposed) link.send({ type: "ack", seq });
        busy = false;
        const next = pending;
        pending = null;
        if (next && !disposed) void draw(next.data, next.at);
      }
    };
    const unlisten = link.listen((event) => {
      if (disposed) return;
      if (event.type === "state") {
        if (event.state.state === "online") {
          open();
        } else {
          sent = false;
          opened = false;
          clear();
          update({
            connected: false,
            error:
              event.state.state === "failed"
                ? `${event.state.detail} Retry the picture after resolving this.`
                : event.state.state === "offline"
                  ? `${event.state.reason} Retry the picture after resolving this.`
                  : null,
          });
        }
      } else if (event.type === "text") {
        const f = event.frame;
        if (f.type === "opened" || f.type === "geometry_changed") {
          if (f.type === "opened" && f.target_id !== id) return;
          if (f.type === "opened") setReopening(false);
          geometryRevision = f.geometry.revision;
          opened = true;
          clear();
          update({ connected: true, error: null });
          link.send({ type: "request_keyframe" });
          adjust.current?.();
        } else if (f.type === "resumed") {
          update({ error: null });
          recover();
        } else {
          update({ connected: false, error: f.reason });
          if (f.type === "ended") {
            opened = false;
            clear();
          }
        }
      } else {
        const next = { data: event.data, at: Date.now() };
        if (busy) {
          if (pending) {
            // A discarded delta breaks the base. Keep only the newest frame,
            // acknowledge the discarded bytes and ask for a complete picture.
            try {
              link.send({
                type: "ack",
                seq: parseTileFrame(pending.data).header.seq,
              });
            } catch {
              /* Recovery below. */
            }
            recover();
          }
          pending = next;
        } else void draw(next.data, next.at);
      }
    });
    open();
    const timeout = setTimeout(() => {
      if (!disposed && latest.current.at === null && !latest.current.error)
        update({ error: "The computer hasn’t sent a picture yet." });
    }, 8_000);
    return () => {
      disposed = true;
      adjust.current = null;
      epoch++;
      clearTimeout(timeout);
      unlisten();
      link.send({ type: "close" });
      link.close();
    };
  }, [
    id,
    visible,
    online,
    captureAllowed,
    platform,
    profile.profileId,
    attempt,
  ]);
  useEffect(() => () => clearTimeout(settle.current), []);
  /** Show only this part of the target, or all of it. */
  const setRegion = useCallback((region: StreamRegion | null) => {
    if (sameRegion(region, wanted.current)) return;
    wanted.current = region;
    clearTimeout(settle.current);
    settle.current = setTimeout(() => adjust.current?.(), REGION_SETTLE_MS);
  }, []);
  const retry = (explicitReopen: boolean) => {
    reopenRequested.current = explicitReopen;
    setReopening(explicitReopen);
    setAttempt((n) => n + 1);
  };
  return {
    canvas,
    backdrop,
    setRegion,
    picture,
    latest,
    reopen: () => retry(true),
    retry: () => retry(!!latest.current.error?.includes("Display changed")),
  };
}
