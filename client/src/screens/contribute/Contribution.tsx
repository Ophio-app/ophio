// SPDX-License-Identifier: Apache-2.0
//
// A handoff from Clankerhub (CH-016, OI-01-U): where it goes, then the
// rules and the person's own check (on the rules screen), then getting the
// source, then a paused project. Opening a project is not starting work:
// nothing here runs the game's code, starts an agent or spends model usage.
// Every state says what happened, what didn't, and what to do next.

import { Check, CircleAlert, Gamepad2, Minus, OctagonX, TriangleAlert, type LucideIcon } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { draftNewTask } from "../../app/github";
import { shallowEqual, useApp, useHost } from "../../app/context";
import { PRODUCT_NAME as APP } from "../../brand";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { Pill } from "../../design/Chip";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { clockTime } from "../../design/time";
import { toast } from "../../design/Toast";
import type { ContributionContext, ContributionView } from "../../protocol/types";
import { Screen } from "../../shell/Screen";
import { useQueue } from "../work/useQueue";
import type { RouteProps } from "../types";
import { blockerFor, repositoryLabel, shortRevision, stageOf, type Blocker, type Facts, type Remedy } from "./codes";
import { expiry, handoffs, type Handoff } from "./handoffs";
import { useContribution, type Contribution as Flow } from "./useContribution";
import { SaveWarning } from "./SaveWarning";
import "./contribute.css";

export const FLOW: ("contribution" | "contribution_rules")[] = ["contribution", "contribution_rules"];
const TITLE = "From Clankerhub";
/** Review's starting words for a first plan: the person's to change, and
 * nothing from the handoff, which a game could use to write their request. */
const PLAN_FIRST = "Read this game’s contribution rules and agent instructions, then suggest a small first change for me to review.";

export default function Contribution({ route }: RouteProps<"contribution">) {
  const c = useContribution(route.handoffId);
  const { profile, ui } = useApp();
  const [sheet, setSheet] = useState<null | "computer" | "folder">(null);
  const h = c.handoff;
  const stage = c.refused && !c.view ? "blocked" : c.view ? stageOf(c.view) : "review";
  // State changes are announced once, as the heading then the body would read.
  const spoken = useRef<string | null>(null);
  const heading = h ? headingOf(c, stage, profile.name) : null;
  useEffect(() => {
    if (!heading || spoken.current === heading) return;
    if (spoken.current !== null) announce(heading);
    spoken.current = heading;
  }, [heading]);
  if (!h) return <Gone />;
  const facts: Facts = { computer: profile.name, folder: c.folder?.label ?? null, started: h.started || h.unsettled !== null };
  const leave = () => ui.finish(FLOW);
  const dismiss = () => { leave(); handoffs().remove(h.id); };
  const act = (remedy: Remedy) => {
    switch (remedy) {
      case "review": void c.review(); break;
      case "check": ui.push({ screen: "contribution_rules", handoffId: h.id }); break;
      case "folder": setSheet("folder"); break;
      case "computer": setSheet("computer"); break;
      case "updates": ui.push({ screen: "settings", section: "settings-versions" }); break;
      case "dismiss": dismiss(); break;
    }
  };
  const sheets = <>
    <ComputerSheet open={sheet === "computer"} onClose={() => setSheet(null)} handoffId={h.id} />
    <FolderSheet open={sheet === "folder"} onClose={() => setSheet(null)} c={c} />
  </>;
  const body = stage === "progress" ? <Progress c={c} />
    : stage === "ready" ? <Ready view={c.view!} />
    : stage === "canceled" ? <Canceled c={c} computer={profile.name} />
    : stage === "reconcile" ? <Reconcile />
    : stage === "blocked" ? <BlockerView blocker={c.refused === "permission" ? denied(profile.name) : blockerFor(c.refused ?? c.view!.code, facts)} />
    : <Review c={c} computer={profile.name} onChoose={setSheet} />;
  const footer = stage === "progress" ? <ProgressFoot c={c} />
    : stage === "ready" ? <ReadyFoot view={c.view!} onDone={dismiss} />
    : stage === "canceled" ? <CanceledFoot c={c} onDone={dismiss} />
    : stage === "reconcile" ? <Foot><Button variant="primary" block onClick={() => act("check")}>Resume</Button><Button variant="quiet" block onClick={leave}>Not now</Button></Foot>
    : stage === "blocked" ? <BlockerFoot blocker={c.refused === "permission" ? denied(profile.name) : blockerFor(c.refused ?? c.view!.code, facts)} act={act} pending={c.pending !== null} leave={leave} />
    : <ReviewFoot c={c} handoff={h} onChoose={setSheet} leave={leave} />;
  return (
    <Screen title={TITLE} footer={footer} footerFlows>
      <div className="contribute"><SaveWarning />{body}</div>
      {sheets}
    </Screen>
  );
}

/** What the screen leads with now, for the announcement. */
function headingOf(c: Flow, stage: string, computer: string): string {
  if (stage === "progress") return "Getting the source";
  if (stage === "ready") return c.view?.code === "reopened" ? "Back in your project" : "Ready. Nothing has run yet.";
  if (stage === "canceled") return "Canceled";
  if (stage === "reconcile") return "Checking what finished";
  if (stage === "blocked") return (c.refused === "permission" ? denied(computer) : blockerFor(c.refused ?? c.view!.code, { computer, folder: null, started: false })).heading;
  return "Open a game from Clankerhub?";
}

function Gone() {
  const { ui } = useApp();
  return (
    <Screen title={TITLE}>
      <Empty icon={Gamepad2} title="This handoff isn’t on this device any more"
        action={<Button onClick={() => ui.finish(FLOW)}>Close</Button>}>
        It was opened, dismissed or it expired. To open the game again, press Open in Ophio on its Clankerhub page.
      </Empty>
    </Screen>
  );
}

const denied = (computer: string): Blocker => ({
  heading: `This device can’t add projects on ${computer}`,
  body: `Opening a game needs this device’s Manage permission on ${computer}. Nothing was downloaded.`,
  primary: { label: "Dismiss", remedy: "dismiss" },
  tone: "warn",
});

function Foot({ children, note }: { children: ReactNode; note?: ReactNode }) {
  return <div className="contribute-foot">{children}{note}</div>;
}

// ---- Before getting the source -----------------------------------------------

/** Values from the handoff, shown as data apart from the copy. */
function Summary({ context, gameId }: { context: ContributionContext | null; gameId: string | null }) {
  const game = context?.game_id ?? gameId;
  return (
    <dl className="group contribute-facts">
      {game && <Fact label="Game"><span className="mono">{game}</span></Fact>}
      {context ? <>
        <Fact label="Source"><span className="mono">{repositoryLabel(context.repository_url)}</span></Fact>
        <Fact label="Revision"><span className="mono" title={context.commit}>{shortRevision(context.commit)}</span></Fact>
        {context.task && (
          <Fact label="Task"><span className="mono">{context.task.id}</span><span className="contribute-facts__sub">Revision {context.task.revision}</span></Fact>
        )}
      </> : <div className="contribute-facts__wait">{APP} shows what it holds once the computer you choose has checked it.</div>}
    </dl>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return <div className="contribute-facts__row"><dt>{label}</dt><dd>{children}</dd></div>;
}

function Review({ c, computer, onChoose }: { c: Flow; computer: string; onChoose: (s: "computer" | "folder") => void }) {
  const { profiles } = useApp();
  const h = c.handoff!;
  const context = c.view?.context ?? null;
  const policies = context?.policies.length ?? 0;
  const awaiting = c.view?.code === "awaiting_local_approval";
  const { ui } = useApp();
  return <>
    <header className="contribute-head">
      <span className="contribute-head__mark" aria-hidden><Gamepad2 strokeWidth={1.8} /></span>
      <div>
        <h1 className="contribute-head__title">Open a game from Clankerhub?</h1>
        <p className="contribute-head__lead">{awaiting ? `Checked by ${computer}. Not trusted yet.` : "Not checked yet."}</p>
      </div>
    </header>
    <Reach c={c} computer={computer} />
    <Summary context={context} gameId={h.gameId} />
    <Section title={`Before ${APP} opens it`}>
      <div className="group">
        <ListRow title="Computer" meta={computer} chevron={profiles.length > 1} onClick={profiles.length > 1 ? () => onChoose("computer") : undefined}
          ariaLabel={profiles.length > 1 ? `Computer: ${computer}. Choose another` : undefined} />
        <ListRow title="Folder" meta={c.folder?.label ?? (c.folders.length ? "Choose" : "None available")} chevron={c.folders.length > 0}
          onClick={c.folders.length > 0 && c.reach.state === "ready" ? () => onChoose("folder") : undefined}
          ariaLabel={c.folders.length > 0 ? `Folder: ${c.folder?.label ?? "not chosen"}. Choose` : undefined} />
        <ListRow title="Rules" chevron={awaiting} onClick={awaiting ? () => ui.push({ screen: "contribution_rules", handoffId: h.id }) : undefined}
          subtitle={policies ? <Pill tone="warn" className="contribute-wrap">{policies} documents · Not reviewed yet</Pill> : undefined}
          meta={policies ? undefined : "Not checked yet"}
          ariaLabel={awaiting ? `Rules: ${policies} documents, not reviewed yet. Review` : undefined} />
      </div>
    </Section>
    <p className="contribute-panel">
      {APP} downloads the source at this revision into its own folder and adds it as a paused project. It doesn’t run the game’s scripts or start an agent.
    </p>
  </>;
}

/** Why the computer can't take the handoff yet, when it can't. */
function Reach({ c, computer }: { c: Flow; computer: string }) {
  const { ui } = useApp();
  const keeps = c.handoff ? `${APP} keeps this handoff until ${clockTime(expiry(c.handoff))}.` : "";
  // Losing the connection doesn't tell us whether the download started.
  if (c.handoff?.unsettled)
    return <Notice tone="unknown" title="No answer yet">{computer} may have started getting the source. {APP} asks it what happened once it’s connected, and won’t start a second download.</Notice>;
  if (c.sendError === "offline" || c.reach.state === "offline")
    return <Notice tone="warn" title={`Can’t reach ${computer}`}>Nothing was downloaded. {keeps} It checks the handoff again once {computer} is back.</Notice>;
  if (c.sendError === "unknown")
    return <Notice tone="unknown" title="No answer yet">{computer} may have checked this handoff. Try again to ask; it won’t make a second one.</Notice>;
  switch (c.reach.state) {
    case "old":
      return <Notice tone="warn" title={`Update ${APP} on ${computer}`}
        action={<Button size="sm" onClick={() => ui.push({ screen: "settings", section: "settings-versions" })}>Check for updates</Button>}>
        This version can’t open games from Clankerhub. Nothing was downloaded.
      </Notice>;
    case "denied":
      return <Notice tone="warn" title={`This device can’t add projects on ${computer}`}>Opening a game needs this device’s Manage permission there. Nothing was downloaded.</Notice>;
    case "unsupported":
      return <Notice tone="warn" title={`${computer} can’t get game source yet`}>{APP} can’t download contribution projects on {computer}’s system yet. Nothing was downloaded.</Notice>;
    case "checking":
      return <p className="contribute-status" role="status"><span className="contribute-spinner" aria-hidden />Asking {computer}</p>;
    case "ready":
      if (!c.folders.length)
        return <Notice tone="warn" title={`No folder on ${computer} can hold it`}>
          {APP} needs a Library folder it can write to as a whole, that isn’t a project’s own or an agent’s. Nothing was downloaded.
        </Notice>;
      if (c.pending === "review") return <p className="contribute-status" role="status"><span className="contribute-spinner" aria-hidden />{computer} is checking the handoff</p>;
      return null;
  }
}

function ReviewFoot({ c, handoff, onChoose, leave }: { c: Flow; handoff: Handoff; onChoose: (s: "computer" | "folder") => void; leave: () => void }) {
  const { ui, profiles } = useApp();
  const until = <p className="contribute-foot__note">Works until {clockTime(expiry(handoff))}</p>;
  const quiet = <Button variant="quiet" block onClick={leave}>Not now</Button>;
  if (c.sendError === "unknown") return <Foot note={until}><Button variant="primary" block pending={c.pending !== null} onClick={() => void c.review()}>Try again</Button>{quiet}</Foot>;
  if (c.reach.state === "unsupported" && profiles.length > 1)
    return <Foot note={until}><Button variant="primary" block onClick={() => onChoose("computer")}>Choose another computer</Button>{quiet}</Foot>;
  if (c.reach.state === "ready" && c.folders.length && !c.folder)
    return <Foot note={until}><Button variant="primary" block onClick={() => onChoose("folder")}>Choose a folder</Button>{quiet}</Foot>;
  const awaiting = c.view?.code === "awaiting_local_approval" && !handoff.unsettled;
  const checking = c.reach.state === "checking" || c.pending === "review";
  return (
    <Foot note={until}>
      <Button variant="primary" block disabled={!awaiting} pending={checking}
        onClick={() => ui.push({ screen: "contribution_rules", handoffId: handoff.id })}>{checking ? "Checking…" : "Review rules"}</Button>
      {quiet}
    </Foot>
  );
}

function ComputerSheet({ open, onClose, handoffId }: { open: boolean; onClose: () => void; handoffId: string }) {
  const { profiles, profile, switchTo } = useApp();
  return (
    <Sheet open={open} onClose={onClose} title="Choose a computer" subtitle="Where the game’s folder goes">
      <div className="group" role="radiogroup" aria-label="Computer">
        {profiles.map((p) => (
          <ListRow key={p.profileId} title={p.name} checked={p.profileId === profile.profileId}
            onClick={() => { onClose(); if (p.profileId !== profile.profileId) switchTo(p.profileId, { screen: "contribution", handoffId }); }} />
        ))}
      </div>
      <p className="fine">Choosing another computer asks you to approve the download again there.</p>
    </Sheet>
  );
}

function FolderSheet({ open, onClose, c }: { open: boolean; onClose: () => void; c: Flow }) {
  const [choice, setChoice] = useState<string | null>(c.folder?.root_id ?? null);
  useEffect(() => { if (open) setChoice(c.folder?.root_id ?? null); }, [open, c.folder]);
  return (
    <Sheet open={open} onClose={onClose} title="Where should contribution projects go?"
      footer={<Button variant="primary" block disabled={!choice} onClick={() => { if (choice) c.setFolder(choice); onClose(); }}>Use this folder</Button>}>
      <p className="contribute-sheet__lead">{APP} puts each game in its own folder here.</p>
      <div className="group" role="radiogroup" aria-label="Folder">
        {c.folders.map((r) => (
          <ListRow key={r.root_id} title={r.label} subtitle={<span className="mono">{r.host_path}</span>} checked={choice === r.root_id} onClick={() => setChoice(r.root_id)} />
        ))}
      </div>
    </Sheet>
  );
}

// ---- Blocked -------------------------------------------------------------------

function BlockerView({ blocker }: { blocker: Blocker }) {
  const Icon: LucideIcon = blocker.tone === "stop" ? OctagonX : TriangleAlert;
  return <>
    <span className={`contribute-badge contribute-badge--${blocker.tone}`} aria-hidden><Icon strokeWidth={2.1} /></span>
    <h1 className="contribute-head__title" tabIndex={-1}>{blocker.heading}</h1>
    <p className="contribute-body">{blocker.body}</p>
    {blocker.note && <p className="contribute-panel">{blocker.note}</p>}
    {blocker.reference && <p className="fine">Reference: <span className="mono">{blocker.reference}</span></p>}
  </>;
}

function BlockerFoot({ blocker, act, pending, leave }: { blocker: Blocker; act: (r: Remedy) => void; pending: boolean; leave: () => void }) {
  const { profiles } = useApp();
  const shown = (r: Remedy) => r !== "computer" || profiles.length > 1;
  const { primary, secondary } = blocker;
  return (
    <Foot>
      {shown(primary.remedy) && <Button variant="primary" block pending={pending && primary.remedy === "review"} onClick={() => act(primary.remedy)}>{primary.label}</Button>}
      {secondary && shown(secondary.remedy) && <Button block onClick={() => act(secondary.remedy)}>{secondary.label}</Button>}
      {primary.remedy !== "dismiss" && secondary?.remedy !== "dismiss" && <Button variant="quiet" block onClick={leave}>Not now</Button>}
    </Foot>
  );
}

// ---- Getting the source --------------------------------------------------------

type Step = "done" | "now" | "waiting";
const STEP_TEXT: Record<Step, string> = { done: "Done", now: "In progress", waiting: "Waiting" };

function Progress({ c }: { c: Flow }) {
  const { profile } = useApp();
  const v = c.view!;
  const steps: [string, Step][] = [
    ["Check the handoff", "done"],
    ["Download the source", v.code === "acquiring" ? "now" : "done"],
    ["Check the rules match", v.code === "verifying" ? "now" : "waiting"],
    ["Add the project", "waiting"],
  ];
  return <>
    <h1 className="contribute-head__title">Getting the source</h1>
    <p className="contribute-status" role="status">
      <span className="contribute-spinner" aria-hidden />
      {v.interruption_pending ? `Stopping. Waiting for ${profile.name} to confirm` : v.code === "acquiring" ? "Downloading the source" : "Checking the rules match"}
    </p>
    <ol className="group contribute-steps" aria-label="Steps">
      {steps.map(([label, s]) => (
        <li key={label} className={`contribute-step contribute-step--${s}`}>
          <span className="contribute-step__mark" aria-hidden>{s === "done" ? <Check strokeWidth={2.6} /> : s === "now" ? <span className="contribute-spinner" /> : null}</span>
          <span className="contribute-step__label">{label}</span>
          <span className="contribute-step__state">{STEP_TEXT[s]}</span>
        </li>
      ))}
    </ol>
    <p className="contribute-body">You can leave this screen. {profile.name} keeps going, and this handoff stays under Tasks until it’s done.</p>
  </>;
}

function ProgressFoot({ c }: { c: Flow }) {
  const stopping = c.pending === "cancel" || !!c.view?.interruption_pending;
  return <Foot><Button block pending={stopping} onClick={() => void c.cancel()}>{stopping ? "Stopping…" : "Cancel"}</Button></Foot>;
}

// ---- Settled -------------------------------------------------------------------

type Mark = "ok" | "warn" | "none";
const MARK: Record<Mark, LucideIcon> = { ok: Check, warn: CircleAlert, none: Minus };

function Readiness({ mark, title, state, detail }: { mark: Mark; title: ReactNode; state: string; detail?: string }) {
  const Icon = MARK[mark];
  return (
    <ListRow leading={<span className={`contribute-mark contribute-mark--${mark}`} aria-hidden><Icon strokeWidth={2.4} /></span>}
      title={title} subtitle={detail} meta={state} />
  );
}

function Ready({ view }: { view: ContributionView }) {
  const project = useHost((s) => (view.project_id ? s.projects[view.project_id] : undefined));
  const { profile } = useApp();
  const back = view.code === "reopened";
  const queue = useQueue(view.project_id);
  const name = project?.name;
  const ctx = view.context;
  return <>
    <h1 className="contribute-head__title">{back ? (name ? `Back in ${name}` : "Back in your project") : name ? `${name} is ready` : "The project is ready"}</h1>
    <div className="contribute-pills">
      <Pill tone="ok" icon={Check}>{back ? "Nothing was changed" : "Nothing has run yet"}</Pill>
      <QueuePill q={queue} />
    </div>
    <p className="contribute-body">
      {back ? `${APP} opened your existing project as you left it.` : `${APP} added it as a project on ${profile.name}.`}
      {view.dirty ? " Its changes are untouched." : ""}
    </p>
    <QueueNote q={queue} />
    <Section title="Ready to work">
      <div className="group">
        {ctx && <Readiness mark="ok" title={<>Source at <span className="mono">{shortRevision(ctx.commit)}</span></>} state="Ready" />}
        <Readiness mark="ok" title="Rules match the handoff you checked" state="Ready" />
        {ctx?.task && <Readiness mark="warn" title={<>Task <span className="mono">{ctx.task.id}</span></>} state="Not attached"
          detail={`${APP} can’t bring in this task’s details yet. Opening it didn’t claim it.`} />}
        <Readiness mark="none" title="Setup" state="Not checked"
          detail={`${APP} won’t run the game’s setup steps. If you start setup later, it asks first.`} />
      </div>
    </Section>
  </>;
}

/** Next up as it is now: a reopened project keeps whatever its queue was
 * doing, so the phone never assumes it's paused. */
function QueuePill({ q }: { q: ReturnType<typeof useQueue> }) {
  if (!q.queue) return <Pill>{q.allowed && !q.error ? "Next up: checking" : "Next up: not known"}</Pill>;
  return q.queue.paused ? <Pill>Queue paused</Pill> : <Pill tone="warn" icon={CircleAlert}>Queue running</Pill>;
}

function QueueNote({ q }: { q: ReturnType<typeof useQueue> }) {
  if (!q.queue || q.queue.paused) return null;
  return <Notice tone="warn" title="Next up is running in this project">It was already on, so tasks waiting there can start. Opening the game didn’t change it.</Notice>;
}

function ReadyFoot({ view, onDone }: { view: ContributionView; onDone: () => void }) {
  const { ui } = useApp();
  const known = useHost((s) => !!view.project_id && !!s.projects[view.project_id]);
  const id = view.project_id;
  const open = () => { if (!id) return; onDone(); ui.selectProject(id); };
  const plan = () => {
    if (!id) return;
    onDone();
    try { draftNewTask(ui, id, { text: PLAN_FIRST, title: "Plan a first contribution", mode: "plan" }); }
    catch (e) { toast(e instanceof Error ? e.message : "Couldn’t open Review.", { tone: "stop" }); }
  };
  return (
    <Foot note={<p className="contribute-foot__note">Planning opens Review. Nothing starts until you choose Start there.</p>}>
      <Button variant="primary" block disabled={!known} onClick={plan}>Plan something</Button>
      <Button block disabled={!known} onClick={open}>Open project</Button>
      <Button variant="quiet" block onClick={onDone}>Done</Button>
    </Foot>
  );
}

function Canceled({ c, computer }: { c: Flow; computer: string }) {
  const v = c.view!;
  const added = !!v.project_id;
  return <>
    <h1 className="contribute-head__title">{added ? "Canceled after it was added" : "Canceled"}</h1>
    {v.interruption_pending && <p className="contribute-status" role="status"><span className="contribute-spinner" aria-hidden />Waiting for {computer} to confirm it stopped</p>}
    <p className="contribute-body">
      {added ? "The project had already been added. It’s paused and nothing has run."
        : c.handoff?.started ? `${APP} stopped getting the source. Anything it got so far stays in its own folder on ${computer}. Nothing was added or run.`
        : "Nothing was downloaded."}
    </p>
  </>;
}

function CanceledFoot({ c, onDone }: { c: Flow; onDone: () => void }) {
  const { ui } = useApp();
  const id = c.view?.project_id;
  const known = useHost((s) => !!id && !!s.projects[id], shallowEqual);
  return (
    <Foot>
      {id && <Button variant="primary" block disabled={!known} onClick={() => { onDone(); ui.selectProject(id); }}>Open project</Button>}
      <Button variant={id ? "secondary" : "primary"} block disabled={!!c.view?.interruption_pending} onClick={onDone}>Done</Button>
    </Foot>
  );
}

function Reconcile() {
  return <>
    <h1 className="contribute-head__title">Checking what finished</h1>
    <p className="contribute-body">
      {APP} isn’t sure the last attempt finished adding the project. When you approve it again, {APP} checks what’s there before downloading anything. You won’t get a duplicate.
    </p>
  </>;
}
