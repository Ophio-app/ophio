// SPDX-License-Identifier: Apache-2.0
//
// What an issue under Needs you asks of the person, at the top of its sheet:
// what happened, what the agent found in its own words, and what to do next.
// Closing it is one tap, in the person's name.

import { useState, type ReactNode } from "react";
import { useApp, useHost, useLink } from "../../app/context";
import { expectItem, useGithubAct } from "../../app/github";
import { GITHUB_CLOSE_FEATURE } from "../../app/githubFeature";
import type { Need } from "../../app/githubNeed";
import { Button } from "../../design/Button";
import { Markdown } from "../../design/Markdown";
import { Notice } from "../../design/Notice";
import { toast } from "../../design/Toast";
import type { GithubItem, GithubProjectView, Project } from "../../protocol/types";
import { useMay } from "../../shell/NotAllowed";

/** Past this many characters, what the agent found is cut to a few lines until asked. */
const LONG = 360;

export function NeedCard({ need, ask, action }: { need: Need; ask: string; action?: ReactNode }) {
  const { platform } = useApp();
  const [all, setAll] = useState(false);
  const long = (need.said?.length ?? 0) > LONG;
  return (
    <Notice tone="warn" className="github-need" title={need.headline} action={action}>
      {need.said && <>
        <p className="github-need__label">{need.saidBy}</p>
        <div className={["github-need__said", long && !all && "github-need__said--cut"].filter(Boolean).join(" ")}>
          <Markdown text={need.said} onLink={(url) => void platform.openExternal(url)} />
        </div>
        {long && <Button variant="quiet" size="sm" aria-expanded={all} onClick={() => setAll(!all)}>{all ? "Show less" : "Show all"}</Button>}
      </>}
      <p className="github-need__label">What to do</p>
      <p>{ask}</p>
    </Notice>
  );
}

const CLOSING = "Closing an issue";

/**
 * Closes `item` on GitHub in the person's name, as done or as not planned.
 * It leaves the list at once and GitHub closes it after; if GitHub refuses,
 * it comes back with why. Where the computer can't yet, `supported` is
 * false and GitHub does it.
 */
export function useCloseIssue(project: Project, view: GithubProjectView, item: GithubItem) {
  const { session } = useApp();
  const online = useLink().state === "online";
  const allowed = useMay("tasks");
  const able = useHost((s) => !!s.capabilities?.features.includes(GITHUB_CLOSE_FEATURE));
  const act = useGithubAct();
  const repo = view.repo ? `${view.repo.owner}/${view.repo.name}` : null;
  const close = (done: boolean): boolean => {
    if (!repo) return false;
    const settled = expectItem(session, project.project_id, item.number, { closed: true });
    toast(`Closed #${item.number}`);
    act({ action: "close_issue", project_id: project.project_id, repo, number: item.number, done }, CLOSING).then(
      () => settled(true),
      (e: unknown) => {
        settled(false);
        toast(`#${item.number} is still open. ${e instanceof Error ? e.message : String(e)}`, { tone: "stop" });
      });
    return true;
  };
  return { supported: able && !!repo, disabled: !online || !allowed, close };
}
