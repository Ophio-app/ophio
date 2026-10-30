// SPDX-License-Identifier: Apache-2.0
//
// The ways into a Git project’s GitHub from Work, among the project’s doors:
// Issues & pull requests, to work and file issues and to check and merge the
// agents' pull requests, one switch apart, then Find & fix, to review the
// project, fix what's found and sort its issues. Turning GitHub on or off is
// in Project settings. Nothing here reads GitHub until one is opened. A
// project that can't use GitHub still has Find & fix, kept in a file.

import { GitPullRequest, ListChecks } from "lucide-react";
import { useApp, useHost } from "../../app/context";
import { FINDINGS_FILE, useFindingsSupported } from "../../app/findings";
import { useGithubSupported } from "../../app/github";
import { prNeedLine } from "../../app/review";
import type { Tab } from "../../app/ui";
import { Segmented } from "../../design/Chip";
import { ListRow } from "../../design/ListRow";
import type { Project, Task } from "../../protocol/types";
import { projectTasks } from "../../state/projection";

const linkable = (project: Project) => project.vcs?.kind === "git" && !project.one_off;

/** Whether this project can be linked to GitHub here. */
export function useGithubLinkable(project: Project): boolean {
  return useGithubSupported() && linkable(project);
}

/** How many pull requests `tasks` have open, each once: a fix and its check share one. */
const openPrs = (tasks: Task[]) => new Set(tasks.flatMap((t) => (t.github?.pr?.state === "open" ? [t.github.pr.number] : []))).size;

/** The agents' open pull requests, how many wait to merge and how many wait for the person, from the tasks already here. */
function usePrCounts(projectId: string): { open: number; merging: number; needs: number } {
  const open = useHost((s) => openPrs(projectTasks(s, projectId)));
  const merging = useHost((s) => openPrs(projectTasks(s, projectId).filter((t) => t.github?.stage === "merging")));
  const needs = useHost((s) => openPrs(projectTasks(s, projectId).filter((t) => t.state !== "archived" && !!prNeedLine(t))));
  return { open, merging, needs };
}

/** Issues & pull requests, then Find & fix; on `tab`, else the tab they're opened from. */
export function GithubDoors({ project, tab }: { project: Project; tab?: Tab }) {
  const { ui } = useApp();
  const github = useGithubLinkable(project);
  const findings = useFindingsSupported();
  const prs = usePrCounts(project.project_id);
  if (!github) {
    return findings
      ? <ListRow icon={ListChecks} title="Find & fix" subtitle={`Reviews that find problems, and their fixes, in ${FINDINGS_FILE}`} chevron
        onClick={() => ui.push({ screen: "findings", projectId: project.project_id }, tab)} />
      : null;
  }
  // The agents' open pull requests are what's likely to need a look, so
  // with any open the door goes to them; with none, to the issues.
  const subtitle = !prs.open ? "Open issues and the agents’ pull requests"
    : [`${prs.open === 1 ? "1 of the agents’ pull requests is" : `${prs.open} of the agents’ pull requests are`} open`,
      prs.needs ? `${prs.needs} ${prs.needs === 1 ? "needs" : "need"} you` : null, prs.merging ? `${prs.merging} waiting to merge` : null].filter(Boolean).join(" · ");
  return <>
    <ListRow icon={GitPullRequest} title="Issues & pull requests" subtitle={subtitle} chevron
      onClick={() => ui.push(prs.open ? { screen: "github_list", projectId: project.project_id } : { screen: "github", projectId: project.project_id, view: "issues" }, tab)} />
    <ListRow icon={ListChecks} title="Find & fix" subtitle="Reviews that find problems, and their fixes" chevron
      onClick={() => ui.push({ screen: "github", projectId: project.project_id, view: "find_fix" }, tab)} />
  </>;
}

/** Atop Issues and Pull requests, behind one door: the other, in its place. */
export function IssuesOrPrs({ projectId, shown }: { projectId: string; shown: "issues" | "prs" }) {
  const { ui } = useApp();
  return <Segmented<"issues" | "prs"> label="Show" className="segmented--fit" value={shown}
    options={[{ value: "issues", label: "Issues" }, { value: "prs", label: "Pull requests" }]}
    onChange={(next) => ui.swap(next === "prs" ? { screen: "github_list", projectId } : { screen: "github", projectId, view: "issues" })} />;
}
