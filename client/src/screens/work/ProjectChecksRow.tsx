// SPDX-License-Identifier: Apache-2.0
import { useChecks } from "../../app/checks";
import { CHECKS_UPDATE } from "../../app/checksFeature";
import { useApp } from "../../app/context";
import { ListRow } from "../../design/ListRow";

/** A project-scoped door, including when this computer needs updating. */
export function ProjectChecksRow({ projectId, onClose }: { projectId: string; onClose: () => void }) {
  const { ui } = useApp();
  const checks = useChecks(projectId);
  const project = checks.projects.find(p => p.project_id === projectId);
  const count = project?.approved.checks.length;
  const title = checks.supported && checks.allowed && count !== undefined ? `Checks: ${count || "not set up"}` : "Checks";
  const subtitle = !checks.supported ? CHECKS_UPDATE : !checks.allowed ? "Task permission is needed to see this project’s checks."
    : checks.error ?? (!checks.online ? "Connect to see the latest checks." : !project ? "Loading checks…" : undefined);
  return <ListRow title={title} subtitle={subtitle} chevron onClick={() => {
    onClose();
    ui.selectProject(projectId);
    ui.push({ screen: "library", part: "checks" }, "library");
  }} />;
}
