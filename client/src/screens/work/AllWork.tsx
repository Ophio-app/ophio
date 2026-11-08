// SPDX-License-Identifier: Apache-2.0
//
// Tasks with All computers chosen and no project picked: what needs the
// person on every connected computer, what runs there and the results to
// look at, each row naming its computer and project. Each part of the list
// is shown as its computer's own, so an answer, an Allow or a Stop goes to
// that computer, and opening an item switches to it first. A computer out
// of reach is left out and named, as the header's "1 offline" counts it:
// the phone keeps no copy of its requests, and nothing there can be
// answered until it is back.

import { CloudOff, Inbox } from "lucide-react";
import { useEffect } from "react";
import { PRODUCT_NAME } from "../../brand";
import { shallowEqual, useApp } from "../../app/context";
import { listedEverywhere, outOfReach, useComputerEntries, useEveryComputer, type ComputerEntry } from "../../app/fleet";
import { ReportNeedsYou } from "../../reports/Questions";
import { useAllCounts } from "../../app/allCounts";
import { useTodoBadge } from "../../app/checklist";
import { WaitingHandoffs } from "../contribute/Waiting";
import { OnComputer } from "../../app/onComputer";
import { Empty, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import type { LinkState } from "../../host/link";
import type { Task } from "../../protocol/types";
import { ContextHeader } from "../../shell/ContextHeader";
import { Screen, useRoute } from "../../shell/Screen";
import { isActiveTask, type HostState } from "../../state/projection";
import { AttentionList, TaskRow } from "./shared";
import { SignInNeedsYou } from "../settings/SignInAgain";
import "./work.css";

interface Reach {
  entry: ComputerEntry;
  online: boolean;
  /** Out of reach, as the header counts it. */
  offline: boolean;
  /** Why it won't come back by itself, when it won't. */
  failed: Extract<LinkState, { state: "failed" }>["reason"] | null;
  /** The phone's app is the older side of a version mismatch. */
  phoneOutdated: boolean;
  needs: number;
  results: number;
  running: Task[];
}

const reach = (entry: ComputerEntry, s: HostState | null, link: LinkState): Reach => {
  const online = link.state === "online" && !!s?.caughtUp;
  const needs = online && s ? listedEverywhere(s) : [];
  // Each task shows in one place: one waiting on the person is under Needs
  // you alone, as in a project's Tasks.
  const waiting = new Set(needs.flatMap((a) => a.task_id ?? []));
  return {
    entry, online,
    offline: outOfReach(link),
    failed: link.state === "failed" ? link.reason : null,
    phoneOutdated: link.state === "failed" && link.reason === "incompatible_version" && phoneIsOlder(link.detail),
    needs: needs.length,
    results: online && s ? listedEverywhere(s, true).length : 0,
    running: online && s ? Object.values(s.tasks).filter((t) => !t.conversation && t.state !== "archived" && isActiveTask(t) && !!s.projects[t.project_id] && !waiting.has(t.task_id)) : [],
  };
};
const sameReach = (a: Reach[], b: Reach[]) => a.length === b.length && a.every((x, i) => {
  const y = b[i]!;
  return x.entry === y.entry && x.online === y.online && x.offline === y.offline && x.failed === y.failed && x.phoneOutdated === y.phoneOutdated && x.needs === y.needs && x.results === y.results && shallowEqual(x.running, y.running);
});
/** The parts of a computer's state `reach` reads, so words streaming in
 * don't make it read again. */
const watched = (s: HostState | undefined) => [s?.caughtUp, s?.attention, s?.resolvedAttention, s?.tasks, s?.projects, s?.approvals, s?.runs];

/** Every computer's reach, read again only when what it lists changes. */
export function useReach(): Reach[] {
  return useEveryComputer(useComputerEntries(), reach, sameReach, watched);
}

/** While All's own list is on the screen, every connected computer's
 * requests are on it, and so are their alerts; once the person goes into
 * one computer's chat, screen or files, or another tab, another computer's
 * requests alert as usual. */
export function useAllOnScreen(): void {
  const { computers } = useApp();
  const { active } = useRoute();
  useEffect(() => (active ? computers?.showAll() : undefined), [active, computers]);
}

/** Whether a version mismatch's words say the phone's app is the older
 * side, as the native link words it ("Update the app on this phone"). */
export function phoneIsOlder(detail: string): boolean {
  return /\bthe app on this phone\b|^Update the app\b/i.test(detail);
}

/** What the person can do about a computer that won't come back by itself. */
function remedy(r: Pick<Reach, "failed" | "phoneOutdated">): string | null {
  switch (r.failed) {
    case "revoked": case "not_paired": case "unauthenticated":
      return "no longer accepts this phone. Pair it again from the computer menu to see its work here.";
    case "incompatible_version":
      return r.phoneOutdated
        ? "needs a newer app on this phone. Update the app on this phone to see its work here."
        : `needs an update before this phone can show its work. Update ${PRODUCT_NAME} on it.`;
    default:
      return null;
  }
}

const names = (list: ComputerEntry[]) => list.length === 1 ? <bdi>{list[0]!.profile.name}</bdi>
  : <>{list.slice(0, -1).map((e, i) => <span key={e.profile.profileId}>{i ? ", " : ""}<bdi>{e.profile.name}</bdi></span>)} and <bdi>{list.at(-1)!.profile.name}</bdi></>;

/** Names the computers left out of All's lists, and what brings them back. */
export function LeftOut({ reach, what }: { reach: Reach[]; what: string }) {
  const out = reach.filter((r) => r.offline);
  const stuck = out.filter((r) => remedy(r));
  const away = out.filter((r) => !remedy(r)).map((r) => r.entry);
  const waiting = reach.filter((r) => !r.online && !r.offline).map((r) => r.entry);
  return <>
    {out.length > 0 && <Notice tone="unknown" report={false} icon={CloudOff} title={`${out.length} offline`}>
      {away.length > 0 && <p>{names(away)} {away.length === 1 ? "isn’t" : "aren’t"} connected, so {away.length === 1 ? "its" : "their"} {what} aren’t shown here. They show again once {away.length === 1 ? "it’s" : "they’re"} back.</p>}
      {stuck.map((r) => <p key={r.entry.profile.profileId}><bdi>{r.entry.profile.name}</bdi> {remedy(r)}</p>)}
    </Notice>}
    {waiting.length > 0 && <p className="work-muted" aria-busy="true">Still connecting to {names(waiting)}…</p>}
  </>;
}

export function AllWork() {
  useAllOnScreen();
  const all = useReach();
  const live = all.filter((r) => r.online);
  const needs = live.filter((r) => r.needs > 0);
  const results = live.filter((r) => r.results > 0);
  // The tab bar's count is this list's: what waits and the report questions.
  const { counts, readers } = useAllCounts(useTodoBadge);
  // What runs on every computer, the most recently moved first.
  const running = live.flatMap((r) => r.running.map((task) => ({ entry: r.entry, task })))
    .sort((a, b) => b.task.updated_at - a.task.updated_at);
  return (
    <Screen header={<ContextHeader heading />} className="work-screen work-root work-all">
      <LeftOut reach={all} what="tasks and requests" />
      {/* Handoffs from Clankerhub are this phone's, waiting for a computer to take them. */}
      <WaitingHandoffs />
      <Section title="Needs you">
        {/* Report questions: the phone's own and those the computer on
            screen carries, then each other computer's, so each shows once. */}
        {/* An agent that isn't signed in, on any computer, each naming its own. */}
        {live.map((r) => <OnComputer key={`sign-in/${r.entry.profile.profileId}`} entry={r.entry}><SignInNeedsYou /></OnComputer>)}
        <ReportNeedsYou />
        {live.filter((r) => !r.entry.current).map((r) => <OnComputer key={`reports/${r.entry.profile.profileId}`} entry={r.entry}><ReportNeedsYou fromComputer /></OnComputer>)}
        {needs.map((r, i) => <OnComputer key={r.entry.profile.profileId} entry={r.entry}><AttentionList projectId={null} lead={i === 0} quiet where /></OnComputer>)}
        {!counts.needs && !counts.asking && <p className="work-muted">{live.length ? "Nothing waiting for you on any computer." : "Nothing to show until a computer connects."}</p>}
      </Section>
      <Section title="Running">
        {running.length > 0
          ? <div className="group">{running.map(({ entry, task }) => <OnComputer key={`${entry.profile.profileId}/${task.task_id}`} entry={entry}><TaskRow task={task} where /></OnComputer>)}</div>
          : <p className="work-muted">No task is running on any computer.</p>}
      </Section>
      {results.length > 0 && <Section title="To look at">
        {results.map((r) => <OnComputer key={r.entry.profile.profileId} entry={r.entry}><AttentionList projectId={null} review quiet where /></OnComputer>)}
      </Section>}
      {!live.length && !all.some((r) => r.offline) && <Empty icon={Inbox} title="Waiting for your computers">Their tasks appear here once they answer.</Empty>}
      {readers}
    </Screen>
  );
}
