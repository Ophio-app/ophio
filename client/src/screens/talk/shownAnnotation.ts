// SPDX-License-Identifier: Apache-2.0
import { draftKey, type UiStore } from "../../app/ui";
import { annotationStore } from "../../state/annotations";
import type { ArtifactMeta } from "../../protocol/types";
import { importSource } from "../annotate/sources";
import { PendingMessageError } from "../control/previewAnnotation";

/** Put a picture or video an agent showed into its task's draft, selected,
 * beside any marks not yet sent. The picture is already on the computer, so
 * sending it back names it instead of uploading it again. */
export async function shownAnnotation(
  ui: UiStore,
  profileId: string,
  at: { projectId: string; taskId: string },
  artifact: Pick<ArtifactMeta, "artifact_id" | "media_type" | "created_at">,
  blob: Blob,
): Promise<string> {
  const composer = ui.draft(at.projectId, at.taskId);
  if (composer?.attempt) throw new PendingMessageError(draftKey(at.projectId, at.taskId));
  const store = annotationStore(profileId);
  const existing = composer?.annotationId ? store.get()[composer.annotationId] : undefined;
  const annotation = existing?.projectId === at.projectId && existing.taskId === at.taskId
    ? existing : store.create(at.projectId, at.taskId);
  const shown = annotation.sources.find((s) => s.origin.kind === "shown" && s.artifactId === artifact.artifact_id);
  const source = shown ?? await importSource(store, annotation.id, {
    blob, mediaType: artifact.media_type, capturedAt: artifact.created_at,
    origin: { kind: "shown", artifact_id: artifact.artifact_id },
  }, { artifactId: artifact.artifact_id });
  store.record(annotation.id, (d) => ({ ...d, selectedSource: source.key, mode: "annotate" }));
  const problem = store.persistence.get();
  if (problem) throw new Error(problem);
  ui.saveDraft(at.projectId, at.taskId, { annotationId: annotation.id });
  if (!ui.persistNow()) throw new Error("The task’s picture could not be saved on this device. Keep this view open and try again.");
  return annotation.id;
}
