// SPDX-License-Identifier: Apache-2.0
import { draftKey, type Draft } from "../../app/ui";
import type { App } from "../../app/context";
import type { AnnotationBundle, Task } from "../../protocol/types";
import { annotationStore, type AnnotationDraft, type SourceDraft } from "../../state/annotations";
import { openRetryDraft } from "../talk/openRetryDraft";
import type { ReceiptRequest } from "./receiptRequests";

export class RetryDraftConflict extends Error {}

/** Whether a draft already holds exactly a request's words and markup. */
export function holdsRequest(draft: Draft | null | undefined, markup: AnnotationDraft | null | undefined, request: Pick<ReceiptRequest, "item">): boolean {
  if (!draft || draft.text !== request.item.text) return false;
  const bundle = request.item.bundle;
  if (!bundle || !draft.annotationId) return !bundle && !draft.annotationId;
  return !!markup && markupOf(markup) === bundleMarkup(bundle);
}

const markupOf = (d: AnnotationDraft) => JSON.stringify([d.sources.map(s => s.artifactId), d.marks, d.comments, d.steps, d.generalNote, d.speech]);
const bundleMarkup = (b: AnnotationBundle) => JSON.stringify([b.sources.map(s => s.artifact_id), b.marks, b.comments, b.steps, b.general_note, b.speech]);

/**
 * Restore the immutable submitted request as a new, reviewable draft. A
 * draft that already holds it is opened as it is, with nothing to choose.
 */
export function retryReceipt(app: App, task: Task, request: Pick<ReceiptRequest, "item">, keepCurrent = false): void {
  const existing = app.ui.draft(task.project_id, task.task_id);
  if (existing?.attempt) throw new Error("Check the task’s pending message before preparing another send.");
  if (existing && holdsRequest(existing, annotationStore(app.profile.profileId).get()[existing.annotationId ?? ""], request)) {
    openRetryDraft(app.ui, task.project_id, task.task_id);
    return;
  }
  if (existing && !keepCurrent) throw new RetryDraftConflict("There’s already a draft for this task.");
  const bundle = request.item.bundle ? structuredClone(request.item.bundle) : null;
  let annotationId: string | null = null;
  if (bundle) {
    const store = annotationStore(app.profile.profileId);
    const draft = store.create(task.project_id, task.task_id);
    const sources: SourceDraft[] = bundle.sources.map(s => ({ key: s.key, blobKey: null, artifactId: s.artifact_id,
      sha256: s.sha256, mediaType: s.media_type, width: s.width, height: s.height, durationMs: null,
      origin: s.origin, capturedAt: s.captured_at, derivedFrom: s.derived_from, derivation: s.derivation }));
    store.update(draft.id, d => ({ ...d, sources, marks: bundle.marks, comments: bundle.comments, steps: bundle.steps,
      generalNote: bundle.general_note, speech: bundle.speech,
      nextSource: Math.max(0, ...sources.map(s => Number(s.key.slice(1)) || 0)) + 1 }));
    if (store.persistence.get()) throw new Error(store.persistence.get()!);
    annotationId = draft.id;
  }
  const key = draftKey(task.project_id, task.task_id);
  app.ui.set(s => ({ ...s, savedDrafts: existing ? { ...s.savedDrafts, [key]: [...(s.savedDrafts[key] ?? []), existing] } : s.savedDrafts,
    drafts: { ...s.drafts, [key]: { projectId: task.project_id, taskId: task.task_id, text: request.item.text, annotationId, updatedAt: Date.now() } } }));
  if (!app.ui.persistNow()) throw new Error("The retry draft could not be saved on this device.");
  openRetryDraft(app.ui, task.project_id, task.task_id);
}
