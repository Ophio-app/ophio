// SPDX-License-Identifier: Apache-2.0
//
// Work's New, at the top of its root: a task, or a find & fix, and for a
// project that can use GitHub, an issue or a pull request too, each going
// straight to its own screen, and on a phone with a camera, a Clankerhub
// code to scan. A new task or pull request opens in Talk's
// chat, and Back from it comes here. While the project's GitHub isn't on,
// Set up GitHub sits directly under New, in a project whose folder isn't a
// Git repository too, to say why and point to the one that is. The
// project's checklist of what the person has to do comes first under New, or
// Request checklist while it has none.

import { useState } from "react";
import { CircleDot, GitPullRequest, ListChecks, Plus } from "lucide-react";
import { useApp } from "../../app/context";
import { FINDINGS_FILE, useFindingsSupported } from "../../app/findings";
import { useGithubIssuesSupported, useGithubProject, useGithubSupported } from "../../app/github";
import { GITHUB_ISSUES_UPDATE } from "../../app/githubFeature";
import { freshReview, type UiStore } from "../../app/ui";
import { PRODUCT_NAME } from "../../brand";
import { ListRow } from "../../design/ListRow";
import { Sheet } from "../../design/Sheet";
import type { Project } from "../../protocol/types";
import { useGithubLinkable } from "../github/GithubDoors";
import { ChecklistRow } from "../checklist/RequestChecklist";
import { GithubSetupRow } from "../github/GithubSettings";
import { ScanClankerhub } from "../contribute/Waiting";

/** New, and under it the checklist's row unless Tasks shows that further down. */
export function NewWork({ project, checklist = true }: { project: Project; checklist?: boolean }) {
  const github = useGithubLinkable(project);
  const setup = useGithubSupported() && !project.one_off && !project.home;
  const [choosing, setChoosing] = useState(false);
  return (
    <div className="group work-new">
      <ListRow icon={Plus} title="New" ariaLabel="New" chevron onClick={() => setChoosing(true)}
        subtitle={github ? "A task, a find & fix, an issue or a pull request" : "A task or a find & fix"} />
      {checklist && <ChecklistRow project={project} />}
      {setup && <GithubSetupRow project={project} />}
      <NewSheet open={choosing} onClose={() => setChoosing(false)} project={project} github={github} />
    </div>
  );
}

/** Opens the project's new task in Chat, as Work with a new pull request, or without asking for one. */
export function draftNewKind(ui: UiStore, projectId: string, pr: boolean): void {
  const prev = ui.draft(projectId, null)?.review;
  // Only New pull request asks for one; New task goes back to what the project does.
  if (pr || prev?.github?.type === "new_pr") {
    const review = { ...(prev ?? freshReview()) };
    if (pr) review.github = { type: "new_pr" };
    else delete review.github;
    ui.saveDraft(projectId, null, { review, ...(pr ? { mode: "work" as const } : {}) });
  }
  ui.openTask(projectId, null);
}

/** New's kinds, each going straight to its own screen. */
function NewSheet({ open, onClose, project, github }: { open: boolean; onClose: () => void; project: Project; github: boolean }) {
  const { ui } = useApp();
  const filing = useGithubIssuesSupported();
  const findings = useFindingsSupported();
  const { view } = useGithubProject(open && github ? project.project_id : null);
  const off = view?.settings.mode === "off" || (!!view && !view.repo);
  const pid = project.project_id;
  const go = (then: () => void) => { onClose(); then(); };
  return (
    <Sheet open={open} onClose={onClose} title="New" subtitle={project.name}>
      <div className="group">
        <ListRow icon={Plus} title="New task" subtitle="Ask an agent to do something in this project" chevron
          onClick={() => go(() => draftNewKind(ui, pid, false))} />
        <ListRow icon={ListChecks} title="New find & fix" chevron disabled={!github && !findings}
          subtitle={github ? "Review the project, fix what’s found, or both"
            : findings ? `Review the project, fix what’s found, or both, in ${FINDINGS_FILE}`
              : `Update ${PRODUCT_NAME} on the computer to review and fix without GitHub`}
          onClick={() => go(() => ui.push(github ? { screen: "github", projectId: pid, view: "find_fix" } : { screen: "findings", projectId: pid }))} />
        {github && <>
          <ListRow icon={CircleDot} title="New issue" chevron disabled={!filing}
            subtitle={filing ? "File it on GitHub, then start it now or later" : GITHUB_ISSUES_UPDATE}
            onClick={() => go(() => ui.push({ screen: "github_new", projectId: pid, kind: "issue" }))} />
          <ListRow icon={GitPullRequest} title="New pull request" chevron disabled={off}
            subtitle={off ? "Turn GitHub on first, with Set up GitHub under New" : "An agent does the work on a branch and opens a draft pull request for it"}
            onClick={() => go(() => draftNewKind(ui, pid, true))} />
        </>}
        <ScanClankerhub before={onClose} />
      </div>
    </Sheet>
  );
}
