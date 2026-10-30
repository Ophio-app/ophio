// SPDX-License-Identifier: Apache-2.0
//
// A pull request: its chip under Talk's chat header, and the sheet that shows
// its checks, open review threads and description, opened from there, a
// task's outcome or the project's list of pull requests. Fixing opens Review
// for a new task on the pull request; Mark ready and Merge are the person's
// own, each behind the phone's unlock. Where its task has a review result,
// that leads; a change that needs the person's approval merges only as
// their approval of the exact commit shown, after a sheet that says what it
// affects.

import { useState, type ReactNode } from "react";
import { ExternalLink, GitMerge, GitPullRequest, GitPullRequestDraft } from "lucide-react";
import { useApp, useHost, useLink } from "../../app/context";
import {
  CHECK_WORDS, METHOD_WORDS, STAGE_WORDS, choiceOf, draftGithubTask, prWords, taskPr, useGithubPr, useGithubSupported,
} from "../../app/github";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { Pill, type Tone } from "../../design/Chip";
import { ListRow, Section } from "../../design/ListRow";
import { Markdown } from "../../design/Markdown";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import type { CheckState, GithubItem, PrCheck, PrStage, Task } from "../../protocol/types";
import { useMay } from "../../shell/NotAllowed";
import { WithCode, draftedWords, unlockFor, useGithubRun } from "./common";
import { ApproveSheet, ReviewResultCard, taskReviewOf } from "./ReviewResult";
import { shortCommit } from "../../app/review";
import "./github.css";

/** Stages worth a word beside the pull request's own state. */
const SHOWN_STAGES: PrStage[] = ["babysitting", "checking", "merging", "capped", "not_reproduced"];

/** What the chip says: the pull request at a glance, then where the watcher has it. */
export function prChipWords(task: Task): string | null {
  const pr = taskPr(task.github);
  if (!pr) return null;
  const stage = task.github?.stage;
  return [...prWords(pr), ...(pr.state === "open" && stage && SHOWN_STAGES.includes(stage) ? [STAGE_WORDS[stage]] : [])].join(" · ");
}

export function PrChip({ task }: { task: Task }) {
  const supported = useGithubSupported();
  const [open, setOpen] = useState(false);
  const pr = taskPr(task.github);
  const words = prChipWords(task);
  if (!supported || !pr || !words) return null;
  const Icon = pr.state === "merged" ? GitMerge : pr.draft ? GitPullRequestDraft : GitPullRequest;
  return (
    <>
      <button type="button" className="chat-pr" onClick={() => setOpen(true)} aria-haspopup="dialog">
        <Icon className="chat-pr__icon" aria-hidden />
        <span className="chat-pr__text">{words}</span>
      </button>
      <PrSheet open={open} onClose={() => setOpen(false)} projectId={task.project_id} number={pr.number} task={task} />
    </>
  );
}

const CHECK_ORDER: CheckState[] = ["failed", "pending", "other", "passed"];
const CHECK_TONE: Record<CheckState, Tone> = { failed: "stop", pending: "accent", other: "neutral", passed: "ok" };

/**
 * One pull request in full. From the project's list it has the list's row,
 * to name it until it's read, and the list's own actions, shown first.
 */
export function PrSheet({ open, onClose, projectId, number, task, item, children }: {
  open: boolean; onClose: () => void; projectId: string; number: number; task?: Task; item?: GithubItem; children?: ReactNode;
}) {
  const { platform, ui } = useApp();
  const online = useLink().state === "online";
  const allowed = useMay("tasks");
  const { pr, error, loading, reload } = useGithubPr(open ? projectId : null, number);
  const owner = useHost((s) => (pr?.task_id ? s.tasks[pr.task_id] ?? null : null)) ?? task ?? null;
  const { busy, run } = useGithubRun();
  const [approving, setApproving] = useState(false);
  if (!open) return null;

  const summary = pr?.summary ?? taskPr(task?.github);
  const isOpen = summary?.state === "open";
  const failing = pr ? pr.checks.some((c) => c.state === "failed") : !!summary?.failed;
  const threads = pr?.threads.filter((t) => !t.resolved) ?? [];
  const checks = pr ? [...pr.checks].sort((a, b) => CHECK_ORDER.indexOf(a.state) - CHECK_ORDER.indexOf(b.state)) : [];
  const method = pr?.method ?? "squash";
  const can = online && allowed && !busy;
  const openUrl = (url: string) => void platform.openExternal(url);
  // Approval is for one commit: the one shown. One an older commit needed is checked again when the person merges.
  const result = taskReviewOf(owner, pr?.summary ?? null);
  const approval = owner?.github?.approval ?? null;
  const approves = !!pr && !!approval && result?.approvalHead === pr.summary.head;
  const staleApproval = !!pr && !!approval && approval.head !== pr.summary.head;

  // The fixer's choice is the pull request's own task's, and Review can change it.
  // Words from someone who can't write to the repository begin in Plan.
  const fix = (text: string) => {
    try {
      const drafted = draftGithubTask(ui, projectId, { link: { type: "pr", number }, text, agent: owner && choiceOf(owner), plan: !!item && !item.author_writes });
      onClose();
      announce(draftedWords(drafted));
    } catch (e) { toast(e instanceof Error ? e.message : String(e), { tone: "stop" }); }
  };
  // Both name the repository it was read from, and the companion acts there or not at all.
  const markReady = () => {
    if (!pr) return;
    void run({ action: "mark_pr_ready", project_id: projectId, repo: pr.repo, number }, "Marking it ready", `Pull request #${number} is ready for review`,
      () => unlockFor(platform, `Mark pull request #${number} ready`)).then((ok) => ok && reload());
  };
  const merge = () => {
    if (!pr) return;
    setApproving(false);
    const head = pr.summary.head;
    // Merging approves only what was shown: anything else and the computer asks the person to look again.
    const approval_shown = approves && approval ? approval.areas : [];
    void run({ action: "merge_pr", project_id: projectId, repo: pr.repo, number, expected_head: head, method, approval_shown }, "Merging", `Pull request #${number} merged`,
      () => unlockFor(platform, approves ? `Approve and merge commit ${shortCommit(head)} of pull request #${number}` : `Merge pull request #${number}`)).then(() => reload());
  };

  return (
    <Sheet open onClose={onClose} size="tall" title={pr || item ? `#${number} ${pr?.title ?? item!.title}` : `Pull request #${number}`}
      subtitle={summary ? prWords(summary).slice(1).join(" · ") : item && `Pull request by @${item.author}`}
      footer={(summary ?? item) && <Button variant="quiet" icon={ExternalLink} onClick={() => openUrl((summary ?? item)!.url)}>Open on GitHub</Button>}>
      <div className="settings-stack github-pr">
        {pr && pr.roles.length > 0 && <p className="github-pr__roles">{pr.roles.join(" · ")}</p>}
        {owner && <ReviewResultCard task={owner} pr={pr?.summary ?? null} />}
        {children}
        {error && <Notice tone="unknown" action={<Button size="sm" disabled={!online} onClick={reload}>Try again</Button>}>{error}</Notice>}
        {!pr && !error && (online
          ? <div className="screen__loading" aria-busy="true" aria-label="Loading the pull request" />
          : <Notice tone="unknown">Connect to read the pull request.</Notice>)}
        {pr && isOpen && (
          <div className="github-pr__actions">
            {pr.summary.conflict && <Button variant="secondary" disabled={!can} onClick={() => fix(`Bring pull request #${number} up to date with ${pr.base_branch} and resolve its conflicts.`)}>Resolve conflicts</Button>}
            {failing && <Button variant="secondary" disabled={!can} onClick={() => fix(`Fix the failing checks on pull request #${number}.`)}>Fix failing checks</Button>}
            {threads.length > 0 && <Button variant="secondary" disabled={!can} onClick={() => fix(`Address the open review comments on pull request #${number}.`)}>Address comments</Button>}
            {pr.summary.draft
              ? <Button variant="primary" disabled={!can} pending={busy === "Marking it ready"} onClick={markReady}>Mark ready</Button>
              : <Button variant="primary" icon={GitMerge} disabled={!can || !pr.mergeable} pending={busy === "Merging"}
                onClick={approves ? () => setApproving(true) : merge}>{approves ? "Approve and merge" : METHOD_WORDS[method]}</Button>}
          </div>
        )}
        {pr && isOpen && (
          <p className="settings-meta">
            {pr.summary.draft
              ? "Mark ready asks for review on GitHub. Your phone’s unlock confirms it."
              : staleApproval ? `Merges commit ${shortCommit(pr.summary.head)} into ${pr.base_branch}, and nothing newer. An older commit needed your approval; if this one does too, you’ll be asked to look at it first. Your phone’s unlock confirms it.`
                : approves ? `This change needs your approval. Approving merges commit ${shortCommit(pr.summary.head)} into ${pr.base_branch}, and nothing newer. Your phone’s unlock confirms it.`
                  : `Merges commit ${pr.summary.head.slice(0, 7)} into ${pr.base_branch}, and nothing newer. Your phone’s unlock confirms it.`}
          </p>
        )}
        {pr && isOpen && owner?.github?.stage === "merging" && (
          <Notice tone="info" title="Waiting to merge">Øphio tests it with {pr.base_branch} and the other pull requests waiting, and merges them once nothing in this project is running or waiting in Next up.</Notice>
        )}
        {pr && isOpen && !pr.summary.draft && !pr.mergeable && (
          <Notice tone="warn" title="It can’t merge yet">
            <WithCode text={pr.blocked ?? "GitHub’s rules for this branch don’t pass yet."} />
            {pr.summary.conflict && " Resolve conflicts has an agent bring it up to date; you merge it once it can."}
          </Notice>
        )}
        {pr && checks.length > 0 && (
          <Section title="Checks">
            <div className="group">{checks.map((c) => <CheckRow key={`${c.name}-${c.url ?? ""}`} check={c} onOpen={openUrl} />)}</div>
          </Section>
        )}
        {pr && threads.length > 0 && (
          <Section title="Open review threads">
            <div className="group">
              {threads.map((t) => (
                <ListRow key={t.id} title={t.path ? `${t.path}${t.line ? `:${t.line}` : ""}` : "On the pull request"}
                  subtitle={<>@{t.author}: <span className="github-pr__thread">{t.body}</span></>} onClick={() => openUrl(t.url)} chevron />
              ))}
            </div>
          </Section>
        )}
        {pr && pr.body.trim() && (
          <Section title="Description">
            <div className="github-pr__body"><Markdown text={pr.body} onLink={openUrl} /></div>
          </Section>
        )}
        {loading && pr && <p className="settings-meta" role="status">Checking GitHub…</p>}
      </div>
      {approving && approves && approval && pr && (
        <ApproveSheet approval={approval} number={number} base={pr.base_branch} onClose={() => setApproving(false)} onApprove={merge} />
      )}
    </Sheet>
  );
}

function CheckRow({ check, onOpen }: { check: PrCheck; onOpen: (url: string) => void }) {
  const url = check.url;
  return (
    <ListRow title={check.name} subtitle={check.required ? "Required" : undefined}
      meta={<Pill tone={CHECK_TONE[check.state]}>{CHECK_WORDS[check.state]}</Pill>}
      {...(url ? { onClick: () => onOpen(url), chevron: true } : {})} />
  );
}
