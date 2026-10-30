// SPDX-License-Identifier: Apache-2.0
import { draftKey, type UiStore } from "../../app/ui";
import { annotationStore } from "../../state/annotations";
import { importSource } from "../annotate/sources";
import type { PreviewDraft } from "./previewDrafts";

/** The task's composer holds a send whose outcome is still open; the
 * refusal stands only while that draft does. */
export class PendingMessageError extends Error {
  constructor(readonly draftKey: string) {
    super("Check the task’s pending message before changing its attached pictures.");
  }
}

/** Bind the retained preview to its original task without replacing unsent marks. */
export async function previewAnnotation(draft: PreviewDraft, ui: UiStore): Promise<string> {
  const composer = ui.draft(draft.projectId, draft.taskId);
  if (composer?.attempt) throw new PendingMessageError(draftKey(draft.projectId, draft.taskId));
  const store = annotationStore(draft.profileId);
  // A failed/held send clears the composer, but its retained markup is still
  // the picture to send again. Reuse it when this task has no newer draft.
  const retained = Object.values(store.get()).filter(d => d.projectId === draft.projectId && d.taskId === draft.taskId &&
    d.sources.some(s => s.origin.kind === "preview_capture" && s.origin.capture_id === draft.id))
    .sort((a, b) => b.updatedAt - a.updatedAt)[0];
  const existing = composer?.annotationId ? store.get()[composer.annotationId] : retained;
  const annotation = existing?.projectId === draft.projectId && existing.taskId === draft.taskId
    ? existing : store.create(draft.projectId, draft.taskId);
  const originName = `Preview: ${draft.previewLabel ?? "Web preview"} · ${new Date(draft.at).toISOString()}.png`;
  const source = annotation.sources.find(s => (s.origin.kind === "preview_capture" && s.origin.capture_id === draft.id) || (s.origin.kind === "phone_import" && s.origin.original_name === originName));
  if (!source) await importSource(store, annotation.id, {
    blob: draft.blob, mediaType: draft.blob.type, capturedAt: draft.at,
    origin: draft.computerId && draft.computerName ? {
      kind: "preview_capture", capture_id: draft.id, host_id: draft.computerId,
      computer_name: draft.computerName, preview_id: draft.previewId,
      preview_label: draft.previewLabel ?? "Web preview",
      captured_at: draft.captureTimeKnown === false ? null : draft.at,
    } : { kind: "phone_import", original_name: originName },
  }, { artifactId: draft.artifactId ?? null });
  else if (draft.artifactId) store.record(annotation.id, d => ({ ...d,
    sources: d.sources.map(s => s.key === source.key ? { ...s, artifactId: draft.artifactId ?? null } : s),
  }));
  const problem = store.persistence.get();
  if (problem) throw new Error(problem);
  ui.saveDraft(draft.projectId, draft.taskId, { annotationId: annotation.id });
  if (!ui.persistNow()) throw new Error("The task’s picture could not be saved on this device. Keep this view open and try again.");
  return annotation.id;
}
