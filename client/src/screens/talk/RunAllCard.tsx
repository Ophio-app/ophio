// SPDX-License-Identifier: Apache-2.0
//
// A chat's proposal to work through every open issue: one card for the
// project's own Run all, fixed and reviewed by the agents chosen for it once
// on the Issues tab, with one tap to start and one summary at the end, here
// in the chat and as a single notification. It starts nothing the Issues tab
// would not, and when it can't start from here it says why and opens the
// Issues tab, where Run all's own settings are.

import { ListChecks, Play } from "lucide-react";
import { useState } from "react";
import { useApp, useHost, useLink } from "../../app/context";
import { useGithubAct, useGithubProject } from "../../app/github";
import { GITHUB_RUN_FEATURE } from "../../app/githubFeature";
import { Button } from "../../design/Button";
import { Pill } from "../../design/Chip";
import { toast } from "../../design/Toast";
import type { Project, Run, Task, TaskOrigin } from "../../protocol/types";
import { asAgent } from "../github/common";
import { agentLabel } from "./intent";
import type { Suggestion } from "./workCards";

export function RunAllCard({ chat, run, project, index }: { chat: Task; run: Run; project: Project; index: number }) {
  const { ui, platform } = useApp();
  const w = (run.outcome?.report?.new_work?.[index] ?? null) as Suggestion | null;
  const pid = project.project_id;
  const online = useLink().state === "online";
  const supported = useHost((s) => !!s.capabilities?.features.includes(GITHUB_RUN_FEATURE));
  const mayStart = useHost((s) => !!s.grants?.tasks);
  const harnesses = useHost((s) => s.harnesses);
  const github = useGithubProject(project.vcs?.kind === "git" && online && supported ? pid : null);
  const act = useGithubAct();
  const [phase, setPhase] = useState<"idle" | "starting" | "started">("idle");

  const view = github.view;
  const origin: TaskOrigin = { chat: chat.task_id, run: run.run_id, index, auto: false };
  const mine = !!view?.run?.origin && view.run.origin.chat === origin.chat && view.run.origin.run === origin.run && view.run.origin.index === index;
  const going = mine && view?.run?.ended_at == null;
  const flow = view?.settings.workflow;
  const fixWith = flow?.fix_with ?? null;
  const reviewWith = flow?.review_with ?? null;

  // What stops it starting from here, in the order the person would fix it.
  const why = !supported ? "Your computer needs an update to run every issue from here."
    : project.vcs?.kind !== "git" ? `${project.name} isn’t a Git project.`
    : !online ? "Your computer is offline."
    : !mayStart ? "This phone can’t start work on your computer."
    : view === undefined ? null
    : !view ? "GitHub couldn’t be reached for this project."
    : view.settings.mode === "off" ? `GitHub is off for ${project.name}.`
    : !fixWith ? "Choose who fixes them on the Issues tab first."
    : !view.settings.warning_answered ? "Answer how far fixes may go on the Issues tab first."
    : null;

  const who = fixWith
    ? `Fixed by ${agentLabel(harnesses, asAgent(fixWith))}${reviewWith ? `, reviewed by ${agentLabel(harnesses, asAgent(reviewWith))}` : ""}. One summary at the end, here and as one notification.`
    : "Fixes every open issue the agent can take, one after another, with one summary at the end.";

  const start = async () => {
    if (phase !== "idle") return;
    platform.haptic?.("tap");
    setPhase("starting");
    try {
      await act({ action: "run_issues", project_id: pid, step: "start", origin }, "Running all the issues");
      setPhase("started");
      github.reload();
    } catch (e) {
      setPhase("idle");
      toast(e instanceof Error ? e.message : String(e), { tone: "stop" });
    }
  };
  const openIssues = () => ui.push({ screen: "github", projectId: pid, view: "issues" });

  if (!w || chat.state === "archived") return null;
  const shown = mine ? (going ? "going" : "ended") : phase;
  return (
    <section className="work-cards" aria-label="Fix every open issue">
      <div className="work-card-item">
        <div className="work-card-item__text">
          <h4 className="work-card-item__title">{w.title}</h4>
          <p className="work-card-item__line">{who}</p>
          {shown === "idle" && why && <p className="work-card-item__why">{why}</p>}
        </div>
        {shown === "starting" && <Pill tone="accent">Starting</Pill>}
        {(shown === "started" || shown === "going") && <Button size="sm" variant="secondary" icon={ListChecks} onClick={openIssues}>Running. Open Issues</Button>}
        {shown === "ended" && <Button size="sm" variant="secondary" icon={ListChecks} onClick={openIssues}>Done. Open Issues</Button>}
        {shown === "idle" && (why
          ? <Button size="sm" variant="secondary" icon={ListChecks} onClick={openIssues}>Open Issues</Button>
          : <Button size="sm" variant="primary" icon={Play} disabled={view === undefined} onClick={() => void start()}>Run all</Button>)}
      </div>
    </section>
  );
}
