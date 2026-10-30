// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { Button } from "../../design/Button";
import { TextArea } from "../../design/Field";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { Section } from "../../design/ListRow";
import {
  commentUnresolved,
  type AnnotationDraft,
  type AnnotationStore,
} from "../../state/annotations";
import type { Comment } from "../../protocol/types";
import { TargetChips } from "./TargetChips";
import { MAX_SOURCES, momentWords } from "./watch";
export function Comments({
  draft: d,
  store,
  onHighlight,
}: {
  draft: AnnotationDraft;
  store: AnnotationStore;
  onHighlight(ids: string[]): void;
}) {
  const [editing, setEditing] = useState<string | null>(null);
  const edit = d.comments.find((c) => c.comment_id === editing);
  const update = (id: string, fn: (c: Comment) => Comment) =>
    store.update(d.id, (v) => ({
      ...v,
      comments: v.comments.map((c) => (c.comment_id === id ? fn(c) : c)),
    }));
  const targets = (
    id: string,
    ids: string[],
    general: boolean,
    accepted = false,
  ) =>
    store.update(d.id, (v) => ({
      ...v,
      comments: v.comments.map((c) =>
        c.comment_id === id
          ? {
              ...c,
              mark_ids: ids,
              link_origins: ids.map(() =>
                accepted ? "suggested_accepted" : "manual",
              ),
            }
          : c,
      ),
      commentState: {
        ...v.commentState,
        [id]: general ? "general" : ids.length ? "manual" : "unresolved",
      },
    }));
  const labels = (ids: string[]) =>
    ids
      .map(
        (id) => d.marks.find((m) => m.mark_id === id)?.label ?? "Missing mark",
      )
      .join(", ");
  // Every comment has the same buttons, so each one's name says which
  // comment it acts on.
  const which = (c: Comment, index: number) => {
    const text = c.text.trim().replace(/\s+/g, " ");
    const cut = text.lastIndexOf(" ", 40);
    return `comment ${index + 1}${text ? `: ${text.length > 40 ? `${text.slice(0, cut > 0 ? cut : 40)}…` : text}` : ""}`;
  };
  return (
    <Section title={`Comments · ${d.comments.length}`}>
      {!d.comments.length && (
        <p className="muted">Add a comment to explain what should change.</p>
      )}
      <div className="an-stack">
        {d.comments.map((c, index) => {
          const unresolved = commentUnresolved(d, c),
            suggestions = (d.suggestions?.[c.comment_id] ?? []).filter((id) =>
              d.marks.some((m) => m.mark_id === id && !m.deleted),
            );
          const late =
            c.origin.kind === "spoken"
              ? c.origin.segment_ids
                  .map((id) => d.speechSuggestions?.[id])
                  .filter((s) => !!s)
              : [];
          return (
            <article
              className={`an-comment${unresolved ? " an-comment--unresolved" : ""}`}
              key={c.comment_id}
            >
              <button
                className="an-comment__body"
                onClick={() => onHighlight(c.mark_ids)}
                aria-label={`Highlight ${labels(c.mark_ids) || "general comment"}: ${c.text}`}
              >
                <span className="an-comment__targets">
                  {d.commentState?.[c.comment_id] === "suggested"
                    ? `Suggested: ${labels(suggestions) || "choose targets"}`
                    : unresolved
                      ? `Unresolved · ${labels(c.mark_ids) || "choose targets"}`
                      : labels(c.mark_ids) || "General comment"}
                </span>
                <span>{c.text}</span>
              </button>
              {c.origin.kind === "spoken" && (
                <p className="fine">
                  {c.origin.edited
                    ? "Your edited wording"
                    : d.speech?.segments.some(
                          (s) =>
                            c.origin.kind === "spoken" &&
                            c.origin.segment_ids.includes(s.segment_id) &&
                            !s.is_final,
                        )
                      ? "Provisional transcript"
                      : "Spoken comment"}
                </p>
              )}
              {c.moment && (
                <p className="fine">{`At ${momentWords(c.moment)} in the video · still ${c.moment.still}`}</p>
              )}
              {!c.moment && d.momentless?.[c.comment_id] && (
                <p className="fine">
                  {d.momentless[c.comment_id]!.why === "full"
                    ? `Goes without a still from the video: the message already has ${MAX_SOURCES} images`
                    : "Goes without a still from the video: its frame couldn’t be taken"}
                </p>
              )}
              {late.map((s) => (
                <Notice key={s.segment_id} title="Later transcript suggestion">
                  {s.text}
                  <Button
                    variant="quiet"
                    onClick={() => {
                      update(c.comment_id, (v) => ({ ...v, text: s.text }));
                      store.record(d.id, (v) => {
                        const suggestions = { ...v.speechSuggestions };
                        delete suggestions[s.segment_id];
                        return { ...v, speechSuggestions: suggestions };
                      });
                    }}
                  >
                    Use this wording
                  </Button>
                </Notice>
              ))}
              <div className="an-wrap">
                {d.commentState?.[c.comment_id] === "suggested" &&
                  suggestions.length > 0 && (
                    <Button
                      variant="quiet"
                      aria-label={`Confirm targets for ${which(c, index)}`}
                      onClick={() =>
                        targets(c.comment_id, suggestions, false, true)
                      }
                    >
                      Confirm
                    </Button>
                  )}
                <Button
                  variant="quiet"
                  aria-label={
                    unresolved
                      ? `Select targets for ${which(c, index)}`
                      : `Edit ${which(c, index)}`
                  }
                  onClick={() => setEditing(c.comment_id)}
                >
                  {unresolved ? "Select targets" : "Edit comment"}
                </Button>
                {unresolved && (
                  <Button
                    variant="quiet"
                    aria-label={`Make general, ${which(c, index)}`}
                    onClick={() => targets(c.comment_id, [], true)}
                  >
                    Make general
                  </Button>
                )}
              </div>
            </article>
          );
        })}
      </div>
      <Sheet
        open={!!edit}
        onClose={() => setEditing(null)}
        title="Edit comment"
        footer={
          <Button variant="primary" onClick={() => setEditing(null)}>
            Keep changes
          </Button>
        }
      >
        {edit && (
          <div className="an-stack">
            <TargetChips
              marks={d.marks}
              targets={edit.mark_ids}
              general={d.commentState?.[edit.comment_id] === "general"}
              onChange={(ids, general) =>
                targets(edit.comment_id, ids, general)
              }
            />
            <TextArea
              label="Comment text"
              value={edit.text}
              onChange={(text) =>
                update(edit.comment_id, (c) => ({
                  ...c,
                  text,
                  origin:
                    c.origin.kind === "spoken"
                      ? { ...c.origin, edited: true }
                      : c.origin,
                }))
              }
            />
            <Button
              variant="stop-outline"
              onClick={() => {
                store.update(d.id, (v) => ({
                  ...v,
                  comments: v.comments.filter(
                    (c) => c.comment_id !== edit.comment_id,
                  ),
                  deletedSpeechSegments: [
                    ...(v.deletedSpeechSegments ?? []),
                    ...(edit.origin.kind === "spoken"
                      ? edit.origin.segment_ids
                      : []),
                  ],
                }));
                setEditing(null);
              }}
            >
              Delete comment
            </Button>
            <p className="fine">Undo in the composer restores this comment.</p>
          </div>
        )}
      </Sheet>
    </Section>
  );
}
