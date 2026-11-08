// SPDX-License-Identifier: Apache-2.0
//
// A video the agent showed, played in the drawing so the person can talk
// over it. How it played is kept on the drawing clock, the one speech is
// timed on, and each comment made meanwhile is given where in the video it
// was made and a still from then, so the agent hears what was said when.

import { useEffect, useRef } from "react";
import { Notice } from "../../design/Notice";
import { outgoingSources, type AnnotationDraft, type AnnotationStore, type SourceDraft, type WatchAnchor } from "../../state/annotations";
import { stillAt } from "./sources";
import { useSourceUrl } from "./useSourceUrl";
import { attachMoment, existingStill, leaveMomentless, MAX_SOURCES, SAME_FRAME_MS, unplacedMoments, videoAt } from "./watch";

/** Enough anchors for a long watch with a lot of seeking. */
const KEPT = 200;
/** The player telling where it is this far from where it was thought to be
 * means it stopped or moved without saying. */
const ADRIFT_MS = SAME_FRAME_MS / 2;

export function Watch({ draft: d, store, source, composing }: {
  draft: AnnotationDraft;
  store: AnnotationStore;
  source: SourceDraft;
  composing: boolean;
}) {
  const { url, error } = useSourceUrl(source);
  const video = useRef<HTMLVideoElement>(null);
  const id = d.id, key = source.key;
  // Talking over a video is about the video: a typed comment needs no mark.
  useEffect(() => {
    store.record(id, (v) => v.composer?.targets.length || v.composer?.general ? v
      : { ...v, composer: { text: v.composer?.text ?? "", targets: [], general: true } });
  }, [store, id]);
  useEffect(() => {
    const el = video.current;
    if (!el || !url) return;
    let started = false;
    // Where the picture was last seen, on the drawing clock.
    let seen: { clock: number; atMs: number } | null = null;
    // Seeking, the player says it is where it is going while it still
    // shows where it was, so it is held there until the frame sought is shown.
    const now = () => {
      const v = store.get()[id];
      const clock = Date.now() - (v?.createdAt ?? 0);
      const shown = el.seeking ? v?.watch?.[key]?.at(-1) : undefined;
      return { clock, atMs: (shown && videoAt([shown], clock, Infinity)) ?? Math.round(el.currentTime * 1000) };
    };
    // Playing isn't moving: it may be starting, seeking or waiting for data.
    const moving = () => !el.paused && !el.ended && !el.seeking && el.readyState >= el.HAVE_FUTURE_DATA;
    const anchor = (watching: boolean, stoppedAt?: { clock: number; atMs: number }) => store.record(id, (v) => {
      const rate = el.playbackRate;
      const held: WatchAnchor[] = stoppedAt ? [{ ...stoppedAt, playing: false, rate, watching: true }] : [];
      const here: WatchAnchor = { ...now(), playing: watching && moving(), rate, watching };
      return { ...v, watch: { ...v.watch, [key]: [...(v.watch?.[key] ?? []), ...held, here].slice(-KEPT) } };
    });
    // Nothing is placed in the video before it is first played.
    const changed = (e: Event) => {
      if (e.type === "play") started = true;
      if (started) anchor(true);
      seen = null;
    };
    // A stall or a start the player didn't announce is found as it plays
    // on, and the video is placed where it was last seen.
    const moved = () => {
      const last = store.get()[id]?.watch?.[key]?.at(-1);
      if (!started || el.seeking || !last) return;
      const here = now();
      const thought = videoAt([last], here.clock, Infinity);
      if (thought !== null && Math.abs(thought - here.atMs) > ADRIFT_MS) {
        anchor(true, seen && seen.clock > last.clock ? seen : undefined);
      }
      seen = here;
    };
    const events = ["play", "playing", "waiting", "pause", "seeking", "seeked", "ratechange", "ended"];
    for (const e of events) el.addEventListener(e, changed);
    el.addEventListener("timeupdate", moved);
    return () => {
      for (const e of events) el.removeEventListener(e, changed);
      el.removeEventListener("timeupdate", moved);
      if (started) anchor(false);
    };
  }, [store, id, key, url]);
  // Typing hides the player; it waits for the person.
  useEffect(() => {
    if (composing) video.current?.pause();
  }, [composing]);
  return (
    <>
      <div className="an-source-caption">
        <strong>{source.key} · Video from the agent</strong>
        <span>Talk while it plays; what you say is matched to the moment.</span>
      </div>
      {error ? <Notice tone="stop">{error}</Notice>
        : url && <video ref={video} className="an-video" src={url} controls playsInline />}
    </>
  );
}

/** Finding moments, by draft: the work for a draft is done once at a time,
 * by whoever asks first, and everyone asking waits for the same. */
const placing = new Map<string, Promise<void>>();

/** Gives each comment made while a shown video played its moment and a
 * still from then, one at a time, until none waits in the draft or in a
 * version of it taken to send. One that can't have a still goes as a
 * general comment, and says why. */
export function placeMoments(store: AnnotationStore, id: string): Promise<void> {
  const running = placing.get(id);
  if (running) return running;
  const work = (async () => {
    for (;;) {
      const versions = [...store.taken(id), store.get()[id]];
      const now = versions.find((v) => v && unplacedMoments(v).length);
      const m = now && unplacedMoments(now)[0];
      if (!now || !m) return;
      const room = outgoingSources(now).length < MAX_SOURCES;
      let still: SourceDraft | null = null;
      try {
        // A version sends the still only while a comment of its own waits for this moment.
        const wants = (v: AnnotationDraft) => unplacedMoments(v).some((p) => p.video.key === m.video.key && p.startMs === m.startMs);
        still = existingStill(now, m.video, m.startMs) ?? (room ? await stillAt(store, id, m.video, m.startMs, wants) : null);
      } catch {
        // Left as a general comment below.
      }
      const found = still;
      store.place(id, (v) => {
        const full = outgoingSources(v).length >= MAX_SOURCES;
        // One not tried for lack of room is tried for a version with room when it is back.
        if (!found) return full ? leaveMomentless(v, m.commentId, m.startMs, "full") : room ? leaveMomentless(v, m.commentId, m.startMs, "failed") : v;
        const own = v.sources.find((s) => s.key === found.key);
        // Only a still this version sends can show its moment; one it
        // hasn't is found again when the version is back.
        if (own && own.included !== false) return attachMoment(v, m.commentId, { still: found.key, start_ms: m.startMs, end_ms: m.endMs });
        return full ? leaveMomentless(v, m.commentId, m.startMs, "full") : v;
      });
    }
  })().finally(() => placing.delete(id));
  placing.set(id, work);
  return work;
}

/** Places moments as comments are made, so a still is ready before Send. */
export function useMoments(store: AnnotationStore, d: AnnotationDraft | null | undefined) {
  const id = d?.id;
  // What is waiting, so a new comment or a changed one starts the work.
  const waiting = d ? unplacedMoments(d).map((p) => `${p.commentId}:${p.startMs}:${p.endMs}`).join() : "";
  useEffect(() => {
    if (id && waiting) void placeMoments(store, id);
  }, [store, id, waiting]);
}
