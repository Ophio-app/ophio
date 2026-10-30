// SPDX-License-Identifier: Apache-2.0
import type { AnnotationDraft, SourceDraft } from "../../state/annotations";

export type ClipRange = { start: number; end: number };
export function clipRange(
  start: string,
  end: string,
  duration: number,
): ClipRange | null {
  const a = Number(start),
    b = Number(end);
  return start.trim() &&
    end.trim() &&
    [a, b, duration].every(Number.isFinite) &&
    a >= 0 &&
    b > a &&
    b <= duration
    ? { start: a, end: b }
    : null;
}
export const boundedMoment = (at: number, range: ClipRange): number =>
  Math.min(
    range.end,
    Math.max(range.start, Number.isFinite(at) ? at : range.start),
  );

/** Follow retained derivatives, including a crop of an extracted clip frame. */
export function clipMoment(
  d: AnnotationDraft,
  source: SourceDraft,
): { clip: SourceDraft; atMs: number } | null {
  let current = source;
  const visited = new Set<string>();
  let atMs: number | null = null;
  while (!visited.has(current.key)) {
    visited.add(current.key);
    if (current.derivation?.kind === "frame_from_clip")
      atMs = current.derivation.at_ms;
    if (current.mediaType.startsWith("video/"))
      return atMs === null ? null : { clip: current, atMs };
    const parent = d.sources.find((s) => s.key === current.derivedFrom);
    if (!parent) break;
    current = parent;
  }
  return null;
}
export function selectedRange(
  d: AnnotationDraft,
  clip: SourceDraft,
): ClipRange | null {
  const duration = (clip.durationMs ?? 0) / 1000;
  const edits = d.clipEdits?.[clip.key];
  return clipRange(
    edits?.start ?? "0",
    edits?.end ?? String(duration),
    duration,
  );
}
export function withinRange(atMs: number, range: ClipRange): boolean {
  return (
    atMs >= Math.round(range.start * 1000) &&
    atMs <= Math.round(range.end * 1000)
  );
}
export function reproductionProblem(d: AnnotationDraft): string | null {
  for (const source of d.sources.filter(
    (s) => s.included !== false && s.mediaType.startsWith("image/"),
  )) {
    const moment = clipMoment(d, source);
    if (!moment) continue;
    const range = selectedRange(d, moment.clip);
    if (!range) return `Choose a valid trim range for ${moment.clip.key}.`;
    if (!withinRange(moment.atMs, range))
      return `${source.key} is outside the trim. Apply the trim and review its steps.`;
  }
  for (const step of d.steps) {
    const source = d.sources.find((s) => s.key === step.source_key);
    const moment = source && clipMoment(d, source);
    const range = moment && selectedRange(d, moment.clip);
    if (
      range &&
      step.clip_range_ms &&
      (!withinRange(step.clip_range_ms[0], range) ||
        !withinRange(step.clip_range_ms[1], range))
    )
      return `Step ${step.index} extends outside the trim. Apply the trim before review.`;
  }
  return null;
}

export function stepRange(
  d: AnnotationDraft,
  key: string,
): [number, number] | null {
  const source = d.sources.find((s) => s.key === key);
  const moment = source && clipMoment(d, source);
  const range = moment && selectedRange(d, moment.clip);
  return range
    ? [Math.round(range.start * 1000), Math.round(range.end * 1000)]
    : null;
}

/** Excluded frames and their comments stay recoverable; unresolved steps block sending. */
export function applyClipTrim(
  d: AnnotationDraft,
  key: string,
  range: ClipRange,
): AnnotationDraft {
  const outside = new Set(
    d.sources
      .filter((source) => {
        const moment = clipMoment(d, source);
        return moment?.clip.key === key && !withinRange(moment.atMs, range);
      })
      .map((s) => s.key),
  );
  return {
    ...d,
    sources: d.sources.map((s) =>
      outside.has(s.key) ? { ...s, included: false } : s,
    ),
    steps: d.steps.map((step) => {
      const source = d.sources.find((s) => s.key === step.source_key);
      const moment = source && clipMoment(d, source);
      return moment?.clip.key === key && !outside.has(step.source_key)
        ? {
            ...step,
            clip_range_ms: [
              Math.round(range.start * 1000),
              Math.round(range.end * 1000),
            ] as [number, number],
          }
        : step;
    }),
  };
}
