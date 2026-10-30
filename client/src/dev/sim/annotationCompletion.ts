// SPDX-License-Identifier: Apache-2.0
import { annotationStore } from "../../state/annotations";
import { UiStore } from "../../app/ui";
import { seedAnnotation } from "./annotate";
import type { SimHost } from "./host";

/** Phone-local fixtures only. No native forward or video exporter is simulated. */
export async function annotationCompletion(
  host: SimHost,
  kind: "empty" | "marks" | "comments" | "clip" | "missing" | "changed",
) {
  const id = await seedAnnotation(host, kind === "changed" ? "marks" : kind);
  if (kind === "changed") {
    const store = annotationStore("sim-studio");
    const target = host.targets[0];
    if (!target) throw new Error("Target fixture missing");
    const geometry = {
      ...target.geometry,
      revision: target.geometry.revision + 1,
      scale: 1.5,
    };
    store.record(id, (draft) => ({
      ...draft,
      sources: draft.sources.map((s) => ({
        ...s,
        origin: {
          kind: "host_capture",
          target_id: target.target_id,
          geometry_revision: geometry.revision,
        },
        captureGeometry: {
          target_id: target.target_id,
          geometry,
          captured_at: s.capturedAt,
        },
        geometryChanged: true,
      })),
    }));
  }
  const ui = new UiStore("sim-studio");
  const project = host.projects[0];
  if (!project) throw new Error("Project fixture missing");
  ui.selectProject(project.project_id);
  ui.selectTask(project.project_id, null);
  ui.push({ screen: "annotate", annotationId: id });
  ui.persistNow();
  return id;
}
export function previewCompletion(host: SimHost, empty = false) {
  const project = host.projects[0];
  if (!project) throw new Error("Project fixture missing");
  host.previews = empty
    ? []
    : [
        {
          preview_id: "preview-local",
          project_id: project.project_id,
          label: "Local development preview",
          url: "http://localhost:5173",
          reachable: true,
          checked_at: Date.now(),
        },
      ];
  host.emit({
    type: "previews_changed",
    project_id: project.project_id,
    previews: host.previews,
  });
}
