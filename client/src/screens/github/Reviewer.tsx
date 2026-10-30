// SPDX-License-Identifier: Apache-2.0
//
// An issue's own reviewer: one agent fixes it, another reviews the code
// before it merges or is handed over. Off unless the person turns it on.

import { useId, useState } from "react";
import { Eye } from "lucide-react";
import { useApp, useHost } from "../../app/context";
import { choiceOf, lastRole, useGithubIssuesSupported } from "../../app/github";
import type { AgentChoice } from "../../app/ui";
import { ListRow } from "../../design/ListRow";
import type { HarnessInfo, LaunchChoice, Project } from "../../protocol/types";
import { AgentSheet } from "../talk/AgentSheet";
import { agentLabel, canStart, modeSupport, resolveAgent } from "../talk/intent";
import { asAgent } from "./common";

/**
 * The reviewer last chosen here on this phone, else an agent other than
 * the one fixing it, so a different model reads the code; else the same.
 */
function useReviewerDefault(project: Project, worker: AgentChoice | null): AgentChoice | null {
  const { profile } = useApp();
  const harnesses = useHost((s) => s.harnesses);
  const last = lastRole(profile.profileId, project.project_id, "review");
  if (last) return resolveAgent(project, harnesses, asAgent(last));
  const other = worker && harnesses.find((h) => h.kind !== worker.harness && ready(h));
  return other ? { harness: other.kind, model: null, effort: null } : worker;
}

// A review runs in Plan, so a default has to be able to.
const ready = (h: HarnessInfo) =>
  canStart(h) && h.account.state === "signed_in" && modeSupport(h, "plan").availability.state === "available";

/** A review choice kept somewhere longer-lived than the screen asking it. */
export interface KeptReview {
  on: boolean;
  agent: AgentChoice | null;
  set: (r: { on?: boolean; agent?: AgentChoice | null }) => void;
}

/** The reviewer as chosen, ready for the link; null while none is. */
export function useReviewer(project: Project, worker: AgentChoice | null, kept?: KeptReview) {
  const supported = useGithubIssuesSupported();
  const fallback = useReviewerDefault(project, worker);
  const [onHere, setOnHere] = useState(false);
  const [chosenHere, setChosenHere] = useState<AgentChoice | null>(null);
  const on = kept ? kept.on : onHere;
  const agent = (kept ? kept.agent : chosenHere) ?? fallback;
  const setOn = kept ? (v: boolean) => kept.set({ on: v }) : setOnHere;
  const setAgent = kept ? (a: AgentChoice | null) => kept.set({ agent: a }) : setChosenHere;
  // A check runs read-only with its agent's own launch flags.
  const sent: LaunchChoice | null = supported && on && agent ? choiceOf(agent) : null;
  return { supported, on, setOn, agent, setAgent, sent };
}

/** The switch for another agent's review, and who it is once on. */
export function ReviewerPick({ project, reviewer }: { project: Project; reviewer: ReturnType<typeof useReviewer> }) {
  const harnesses = useHost((s) => s.harnesses);
  const [choosing, setChoosing] = useState(false);
  const hint = useId();
  const { supported, on, setOn, agent, setAgent } = reviewer;
  if (!supported) return null;
  return <>
    <label className="settings-check">
      <input type="checkbox" checked={on} aria-describedby={hint} disabled={!agent} onChange={(e) => setOn(e.target.checked)} />
      Have another agent review the code
    </label>
    <p className="settings-meta" id={hint}>
      {on
        ? "Once its pull request’s checks pass, this agent reviews the change without editing files, before it merges or is handed to you."
        : "Off: uses this project’s setting."}
    </p>
    {on && agent && <>
      <div className="group">
        <ListRow icon={Eye} title="Reviewed by" subtitle={agentLabel(harnesses, agent)} chevron onClick={() => setChoosing(true)} />
      </div>
      <AgentSheet open={choosing} onClose={() => setChoosing(false)} project={project} value={agent} mode="plan"
        launchFlags={false} onChange={setAgent} />
    </>}
  </>;
}
