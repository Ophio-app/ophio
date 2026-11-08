// SPDX-License-Identifier: Apache-2.0
//
// One task's own say over its pull request once its checks pass: it merges
// by itself, or it waits for the person. It starts as the project's setting,
// and is sent only where the person picks otherwise, so a task left alone
// follows the project if that changes.

import { useEffect, useState } from "react";
import { useHost, useLink } from "../../app/context";
import { readOnlyLink, useGithubProject } from "../../app/github";
import { Segmented } from "../../design/Chip";
import type { GithubProjectView, GithubSettings, Task, WhenGreen } from "../../protocol/types";
import { useMay } from "../../shell/NotAllowed";
import { useGithubRun } from "./common";

export const WHEN_GREEN_FEATURE = "github_when_green_v1";
export const WHEN_GREEN_UPDATE = "Update Øphio on this computer to start a task with its own choice about merging.";
export const useWhenGreenSupported = () => useHost((s) => !!s.capabilities?.features.includes(WHEN_GREEN_FEATURE));

/** What the project does by itself: Through merge merges once its warning is answered. */
export const projectWhenGreen = (settings: GithubSettings): WhenGreen =>
  settings.mode === "through_merge" && settings.warning_answered ? "merge" : "leave";

/** Whether a task's pull request merges by itself: its own choice, else the project's. */
export const mergesItself = (whenGreen: WhenGreen | null | undefined, settings: GithubSettings) =>
  (whenGreen ?? projectWhenGreen(settings)) === "merge";

const SAYS: Record<WhenGreen, string> = { merge: "Merge it", leave: "Leave it for me" };

/**
 * Merge it is a merge said yes to ahead of time, so it takes what merging
 * does: an account that administers the repository and this phone's leave
 * to approve. Back to the project's own merging takes only the task.
 */
function useOffered(view: GithubProjectView) {
  const approves = useMay("approvals");
  const byDefault = projectWhenGreen(view.settings);
  const repo = view.repo;
  const merge = byDefault === "merge" || (!!repo?.admin && !!repo.push && approves);
  const why = merge ? null
    : !repo?.admin || !repo.push ? `Merge it is offered where ${view.login ? `@${view.login}` : "the account"} administers the repository.`
    : "This phone may not approve merges, so it can’t choose Merge it.";
  return { byDefault, merge, why };
}

function Words({ value, byDefault, branch, hold }: { value: WhenGreen; byDefault: WhenGreen; branch: string; hold?: string | null }) {
  return <p className="settings-meta">
    {value === "merge" ? hold ?? `It merges into ${branch} by itself once its checks pass and any review is clear, unless it needs your approval.`
      : "It waits for you: you mark it ready and merge it."}
    {value === byDefault && " That’s this project’s setting."}
  </p>;
}

/** The choice for a task about to start; `sent` goes in its link. */
export function useWhenGreen(view: GithubProjectView) {
  const supported = useWhenGreenSupported();
  const offered = useOffered(view);
  const [chosen, setChosen] = useState<WhenGreen | null>(null);
  // A choice no longer offered, as when the project's setting changed under it, falls back.
  const value = chosen === "merge" && !offered.merge ? offered.byDefault : chosen ?? offered.byDefault;
  const sent: WhenGreen | null = supported && value !== offered.byDefault ? value : null;
  // A computer that can't keep the choice would follow the project instead,
  // merging what the person said to leave: it isn't started until they
  // pick the project's setting or update it.
  const blocked = !supported && chosen !== null && value !== offered.byDefault;
  return { supported, value, set: setChosen, sent, view, offered, blocked };
}

export function WhenGreenPick({ choice, disabled }: { choice: ReturnType<typeof useWhenGreen>; disabled?: boolean }) {
  const { supported, value, set, view, offered, blocked } = choice;
  if ((!supported && !blocked) || view.settings.mode === "off") return null;
  return <Pick value={value} byDefault={offered.byDefault} why={offered.why} branch={view.repo?.default_branch ?? "main"}
    options={options(offered.merge, !!disabled)} onChange={set} />;
}

/**
 * A started task's choice, changed in place until its pull request merges.
 * It flips at once and goes back if the computer refuses.
 */
export function TaskWhenGreen({ task }: { task: Task }) {
  const supported = useWhenGreenSupported();
  const github = task.github;
  const shown = supported && !!github && !readOnlyLink(github.link) && !github.from_stranger && github.stage !== "closed" && github.stage !== "merged" && github.pr?.state !== "merged" && github.pr?.state !== "closed";
  const { view } = useGithubProject(shown ? task.project_id : null);
  const online = useLink().state === "online";
  const allowed = useMay("tasks");
  const { busy, run } = useGithubRun();
  const [flipped, setFlipped] = useState<WhenGreen | null>(null);
  const kept = github?.when_green ?? null;
  useEffect(() => setFlipped(null), [kept]);
  if (!shown || !view || view.settings.mode === "off") return null;
  return <TaskPick task={task} view={view} kept={kept} flipped={flipped} setFlipped={setFlipped}
    disabled={!online || !allowed || !!busy} run={run} />;
}

function TaskPick({ task, view, kept, flipped, setFlipped, disabled, run }: {
  task: Task; view: GithubProjectView; kept: WhenGreen | null; flipped: WhenGreen | null; setFlipped: (v: WhenGreen | null) => void;
  disabled: boolean; run: ReturnType<typeof useGithubRun>["run"];
}) {
  const offered = useOffered(view);
  const value = flipped ?? kept ?? offered.byDefault;
  // What holds it now still does: Merge it says what happens after.
  const hold = task.github?.stage === "capped"
    ? "It’s stopped for you to look at, so it doesn’t merge by itself until you pick it up again."
    : task.github?.approval
      ? "This change needs your approval, so you merge it yourself even so."
      : null;
  const change = async (to: WhenGreen) => {
    if (to === value) return;
    setFlipped(to);
    // Back to what the project does is no choice of its own.
    const when_green = to === offered.byDefault ? null : to;
    const ok = await run({ action: "set_when_green", task_id: task.task_id, ...(when_green ? { when_green } : {}) },
      to === "merge" ? "Letting it merge by itself" : "Leaving it for you to merge",
      to === "merge" ? (hold ? "It still waits for you this time" : "It merges by itself once its checks pass") : "It waits for you to merge it");
    if (!ok) setFlipped(null);
  };
  return <Pick value={value} byDefault={offered.byDefault} why={offered.why} branch={view.repo?.default_branch ?? "main"}
    hold={hold} options={options(offered.merge || value === "merge", disabled)} onChange={(to) => void change(to)} />;
}

const options = (merge: boolean, disabled: boolean) => (["merge", "leave"] as const).map((value) => ({
  value, label: SAYS[value], disabled: disabled || (value === "merge" && !merge),
}));

function Pick({ value, byDefault, why, branch, hold, options, onChange }: {
  value: WhenGreen; byDefault: WhenGreen; why: string | null; branch: string; hold?: string | null;
  options: { value: WhenGreen; label: string; disabled: boolean }[]; onChange: (v: WhenGreen) => void;
}) {
  return <div className="settings-stack github-when-green">
    <Segmented className="segmented--fit" label="When its checks pass" value={value} options={options} onChange={onChange} />
    <Words value={value} byDefault={byDefault} branch={branch} hold={hold} />
    {why && value !== "merge" && <p className="settings-meta">{why}</p>}
  </div>;
}
