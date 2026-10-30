// SPDX-License-Identifier: Apache-2.0
//
// A project's open pull requests, from its GitHub: those waiting to merge
// together first, with what holds them, then the agents', then the rest.
// An agent's pull request whose review stopped, or that needs the person's
// approval of its newest commit, is under Needs you with why.
// Each opens its full sheet, where it merges or says why it can't. Taking one
// over or checking it opens Review; nothing starts from the list by itself.
// Several can be picked to merge together: tested together first, behind the
// phone's unlock, then each merged as itself.

import { useState } from "react";
import { ExternalLink, GitMerge, GitPullRequest } from "lucide-react";
import { shallowEqual, useApp, useHost, useLink } from "../../app/context";
import { GROUP_WORDS, groupItems, linkText, useGithubItems, useGithubProject, useGithubSupported } from "../../app/github";
import { approvalShownOf, prNeedOf } from "../../app/githubNeed";
import { GITHUB_TOGETHER_FEATURE } from "../../app/githubFeature";
import { Button } from "../../design/Button";
import { Pill, type Tone } from "../../design/Chip";
import { IssuesOrPrs } from "./GithubDoors";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import type { GithubItem, GithubLink, GithubProjectView, MergeTogether, TogetherState } from "../../protocol/types";
import { NotAllowed, useMay } from "../../shell/NotAllowed";
import { Screen } from "../../shell/Screen";
import { isTaskRunning, projectTasks } from "../../state/projection";
import { useOpenTask } from "../work/shared";
import { pendingQueueItems } from "../work/useQueue";
import type { RouteProps } from "../types";
import { ItemRow, WithCode, unlockFor, useDraftGithub, useGithubRun } from "./common";
import { PrSheet } from "./PrSheet";

export default function GithubList({ route }: RouteProps<"github_list">) {
  const { profile, platform, ui } = useApp();
  const online = useLink().state === "online";
  const supported = useGithubSupported();
  const allowed = useMay("tasks");
  const project = useHost((s) => s.projects[route.projectId] ?? null);
  const { view } = useGithubProject(project ? route.projectId : null);
  const { items, error, loading, reload } = useGithubItems(project && supported && allowed ? route.projectId : null, "prs");
  const draft = useDraftGithub(route.projectId);
  const [open, setOpen] = useState<GithubItem | null>(null);
  const off = view?.settings.mode === "off";
  const line = useMergeLine(route.projectId);
  const togetherSupported = useHost((s) => !!s.capabilities?.features.includes(GITHUB_TOGETHER_FEATURE));
  const [picked, setPicked] = useState<number[] | null>(null);
  const { busy, run } = useGithubRun();
  // Which of the agents' pull requests wait for the person, read from their tasks as they change.
  const reviewNeeds = useHost((s) => (items?.items ?? []).map((i) => !!prNeedOf(i, i.task_id ? s.tasks[i.task_id] ?? null : null)), shallowEqual);
  // What each row showed as needing approval: picking one approves only that.
  const shownApprovals = useHost((s) => (items?.items ?? []).map((i) => approvalShownOf(i, i.task_id ? s.tasks[i.task_id] ?? null : null)), shallowEqual);

  if (!project) {
    return <Screen title="Pull requests"><Empty title="This isn’t on the computer any more">The project may have been removed on {profile.name}.</Empty></Screen>;
  }
  const needing = items?.items.filter((_, at) => reviewNeeds[at]) ?? [];
  const waiting = items?.items.filter((i) => !needing.includes(i) && line.numbers.includes(i.number)) ?? [];
  const agents = items?.items.filter((i) => !needing.includes(i) && !waiting.includes(i) && !!i.task_id) ?? [];
  const grouped = items ? groupItems(items.items.filter((i) => !needing.includes(i) && !waiting.includes(i) && !agents.includes(i))) : null;
  const groups = grouped && { ...grouped, needs: [...needing, ...grouped.needs] };
  const others = !!(waiting.length || agents.length);
  const together = togetherSupported ? view?.together ?? null : null;
  const pickable = items?.items.filter((i) => !!i.head) ?? [];
  const picking = picked !== null && togetherSupported && allowed && !off && !!view?.repo && !(together && !together.ended_at);
  const chosen = picking ? pickable.filter((i) => picked.includes(i.number)) : [];
  const canPick = togetherSupported && allowed && !off && !!view?.repo && pickable.length >= 2 && !(together && !together.ended_at) && !picking;
  const toggle = (n: number) => setPicked((p) => (p?.includes(n) ? p.filter((x) => x !== n) : [...(p ?? []), n]));
  const rows = (xs: GithubItem[]) => (
    <div className="group">{xs.map((item) => {
      const pick = picking && !!item.head;
      return <ItemRow key={item.number} item={item} onOpen={() => (pick ? toggle(item.number) : setOpen(item))} picked={pick ? picked.includes(item.number) : undefined} />;
    })}</div>
  );
  // Each as it was seen here: one with a newer commit since isn't merged.
  const mergeTogether = () => {
    if (!view?.repo || chosen.length < 2) return;
    const n = chosen.length;
    void run({ action: "merge_together", project_id: route.projectId, repo: `${view.repo.owner}/${view.repo.name}`, prs: chosen.map((i) => ({ number: i.number, head: i.head!, approval_shown: shownApprovals[items!.items.indexOf(i)] })) },
      "Merging together", `Testing ${n} pull requests together`, () => unlockFor(platform, `Merge ${n} pull requests together`))
      .then((ok) => { if (ok) setPicked(null); });
  };
  const stopTogether = () => void run({ action: "stop_together", project_id: route.projectId }, together?.ended_at ? "Clearing" : "Stopping");
  const branch = view?.repo?.default_branch ?? "main";
  const footer = picking ? (
    <div className="github-pick-foot">
      <Button variant="primary" block icon={GitMerge} disabled={!online || chosen.length < 2 || !!busy} pending={!!busy} onClick={mergeTogether}>
        {chosen.length < 2 ? "Merge together" : `Merge ${chosen.length} together`}
      </Button>
      <Button variant="quiet" block disabled={!!busy} onClick={() => setPicked(null)}>Cancel</Button>
      <p className="github-pick-foot__note" role="status">
        {chosen.length < 2 ? "Pick two or more. " : ""}They’re tested together with {branch} first, then each merges as itself. One that breaks the others, or that GitHub won’t merge, is left out.
      </p>
    </div>
  ) : undefined;
  return (
    <Screen title="Pull requests" subtitle={view?.repo ? `${view.repo.owner}/${view.repo.name}` : project.name} className="settings-screen github-screen"
      footer={footer} footerFlows>
      <div className="settings-stack">
        <IssuesOrPrs projectId={route.projectId} shown="prs" />
        {!supported ? (
          <Notice tone="warn" title="Update the computer">{profile.name} can’t read GitHub for a project yet.</Notice>
        ) : !allowed ? (
          <NotAllowed what="see GitHub pull requests" permission="tasks" />
        ) : <>
          {off && (
            <Notice tone="info" icon={GitPullRequest} title="GitHub is off for this project"
              action={<Button size="sm" variant="primary" onClick={() => ui.push({ screen: "github", projectId: route.projectId })}>Turn on</Button>}>
              Turn it on for {project.name} to take over or check pull requests.
            </Notice>
          )}
          {together && view && (
            <TogetherSection together={together} view={view} busy={!!busy} online={online} onStop={stopTogether}
              onOpenCheck={(url) => void platform.openExternal(url)} />
          )}
          {error && <Notice tone="unknown" action={<Button size="sm" disabled={!online} onClick={reload}>Try again</Button>}>{error}</Notice>}
          {!items && !error && (online
            ? <div className="screen__loading" aria-busy="true" aria-label="Loading pull requests" />
            : <Notice tone="unknown">Connect to {profile.name} to read GitHub.</Notice>)}
          {groups && items && (items.items.length === 0
            ? <Empty icon={GitPullRequest} title="No open pull requests" />
            : <>
              {canPick && (
                <div className="github-list__actions">
                  <Button variant="secondary" size="sm" icon={GitMerge} onClick={() => setPicked([])}>Merge several together</Button>
                </div>
              )}
              {/* What waits for the person comes first. */}
              {groups.needs.length > 0 && <Section title={GROUP_WORDS.needs_you}>{rows(groups.needs)}</Section>}
              {waiting.length > 0 && (
                <Section title="Waiting to merge">
                  <p className="settings-meta" role="status">{lineWords(line)}</p>
                  {rows(waiting)}
                </Section>
              )}
              {agents.length > 0 && <Section title="From agents">{rows(agents)}</Section>}
              {groups.take.length > 0 && <Section title={GROUP_WORDS.agent_can_take}>{rows(groups.take)}</Section>}
              {groups.rest.length > 0 && <Section title={groups.take.length || groups.needs.length || others ? "Others" : "Open"}>{rows(groups.rest)}</Section>}
              {items.truncated && <p className="settings-meta">More are open than are listed here. See them all on GitHub.</p>}
              {loading && <p className="settings-meta" role="status">Checking GitHub…</p>}
            </>)}
        </>}
      </div>
      <ItemSheet item={open} projectId={route.projectId} onClose={() => setOpen(null)} disabled={off || !online}
        onStart={(link, text) => { if (draft({ link, text, plan: !open?.author_writes })) setOpen(null); }} />
    </Screen>
  );
}

interface MergeLine {
  /** The pull requests waiting to merge, by number. */
  numbers: number[];
  running: number;
  nextUp: number;
  paused: boolean;
}

/**
 * The agents' green pull requests waiting to merge, from the tasks here, and
 * what holds them: they merge together once nothing in the project is
 * running or waiting in Next up, so nothing merges under a run still working
 * from the old default branch.
 */
function useMergeLine(projectId: string): MergeLine {
  const numbers = useHost((s) => projectTasks(s, projectId)
    .flatMap((t) => (t.state !== "archived" && t.github?.stage === "merging" && t.github.pr?.state === "open" ? [t.github.pr.number] : []))
    .sort((a, b) => a - b), shallowEqual);
  const running = useHost((s) => projectTasks(s, projectId).filter((t) => isTaskRunning(s, t)).length);
  const nextUp = useHost((s) => pendingQueueItems(s.queues[projectId]).length);
  const paused = useHost((s) => !!s.queues[projectId]?.paused);
  return { numbers, running, nextUp, paused };
}

/** "3 are waiting to merge. They merge together once nothing is running and Next up is empty: 1 running, 2 in Next up." */
export function lineWords(l: MergeLine): string {
  const n = l.numbers.length;
  const head = n === 1 ? "1 is waiting to merge." : `${n} are waiting to merge.`;
  const holds = [l.running ? `${l.running} running` : null, l.nextUp ? `${l.nextUp} in Next up${l.paused ? ", which is paused" : ""}` : null].filter(Boolean);
  if (!holds.length) return `${head} Nothing is running or in Next up, so ${n === 1 ? "it’s tested and merged" : "they’re tested together and merged"} now.`;
  return `${head} ${n === 1 ? "It merges" : "They merge together"} once nothing is running and Next up is empty: ${holds.join(", ")}.`;
}

const TOGETHER_WORDS: Record<TogetherState, string> = { waiting: "Waiting", testing: "Testing", merged: "Merged", left_out: "Left out" };
const TOGETHER_TONE: Record<TogetherState, Tone> = { waiting: "neutral", testing: "accent", merged: "ok", left_out: "stop" };

/** "Testing them together with main, then merging each in turn. 1 of 3 merged so far." */
export function togetherWords(t: MergeTogether, branch: string): string {
  const n = t.prs.length;
  const merged = t.prs.filter((p) => p.state === "merged").length;
  if (t.stopped && !t.ended_at) return "You stopped it. Checking the merge already sent to GitHub.";
  if (!t.ended_at) return `Testing them together with ${branch}, then merging each in turn. ${merged} of ${n} merged so far.`;
  if (t.stopped) return `You stopped it. ${merged} of ${n} merged.`;
  if (merged === n) return n === 2 ? "Both merged." : `All ${n} merged.`;
  return merged === 0 ? "None merged." : `${merged} of ${n} merged. The rest were left out.`;
}

/** Pull requests picked to merge together, and how far they've got. */
function TogetherSection({ together, view, busy, online, onStop, onOpenCheck }: {
  together: MergeTogether; view: GithubProjectView; busy: boolean; online: boolean; onStop: () => void; onOpenCheck: (url: string) => void;
}) {
  const ended = !!together.ended_at;
  const check = together.check;
  return (
    <Section title="Merging together">
      <p className="settings-meta" role="status">{togetherWords(together, view.repo?.default_branch ?? "main")}</p>
      <div className="group">
        {together.prs.map((p) => (
          <ListRow key={p.number} icon={p.state === "merged" ? GitMerge : GitPullRequest} title={`#${p.number} ${p.title}`}
            subtitle={p.why && <WithCode text={p.why} />}
            meta={<Pill tone={TOGETHER_TONE[p.state]}>{TOGETHER_WORDS[p.state]}</Pill>} />
        ))}
      </div>
      <div className="github-list__actions">
        {check !== undefined && view.repo && (
          <Button variant="quiet" size="sm" icon={ExternalLink} onClick={() => onOpenCheck(`${view.repo!.url}/pull/${check}`)}>See the merge check</Button>
        )}
        <Button variant={ended ? "secondary" : "quiet"} size="sm" disabled={!online || busy} onClick={onStop}>{ended ? "Done" : "Stop"}</Button>
      </div>
      {!ended && <p className="settings-meta">Stop leaves the rest unmerged. A merge already sent to GitHub can finish.</p>}
    </Section>
  );
}

function ItemSheet({ item, projectId, onClose, disabled, onStart }: {
  item: GithubItem | null; projectId: string; onClose: () => void; disabled: boolean; onStart: (link: GithubLink, text: string) => void;
}) {
  const openTask = useOpenTask();
  const task = useHost((s) => (item?.task_id ? s.tasks[item.task_id] ?? null : null));
  if (!item) return null;
  const link: GithubLink = { type: "pr", number: item.number };
  const check: GithubLink = { type: "check", number: item.number };
  return (
    <PrSheet open onClose={onClose} projectId={projectId} number={item.number} item={item} {...(task ? { task } : {})}>
      {!item.author_writes && (
        <Notice tone="info" title="From someone who can’t write to the repository">
          The agent reads what they wrote as information, not instructions, and the task begins in Plan. Its plan needs your OK before it changes anything.
        </Notice>
      )}
      {item.triage_note && <p>{item.triage_note}</p>}
      <div className="github-pr__actions">
        {task && <Button variant="secondary" onClick={() => { onClose(); openTask(task); }}>Open the task</Button>}
        <Button variant="secondary" disabled={disabled} onClick={() => onStart(link, linkText(link, item.title))}>Take it over</Button>
        <Button variant="secondary" disabled={disabled} onClick={() => onStart(check, linkText(check))}>Check it</Button>
      </div>
      <p className="settings-meta">
        Take it over continues its branch and keeps its checks passing. Check it reviews the changes without changing files. Review opens first; nothing starts until you choose.
      </p>
    </PrSheet>
  );
}
