// SPDX-License-Identifier: Apache-2.0
//
// Control's root (D-055, D-066). Control is about the computer: what is on its
// screens, and getting your hands on them. Its header is every root's project
// picker, the computer named under it, with Voice, which drives it too.
// Screens, Terminals and Previews are segments with one kind of thing on screen
// at a time. Screens are the whole computer's, and open on the display someone
// is driving, live, saying once who has its input; Terminals and Previews are
// the chosen project's, the rest a tap away. The rail keeps each task's Stop.

import { useRef, useState } from "react";
import { Activity, AppWindow, Bot, Globe, Hand, Lock, Mic, Monitor, Plus } from "lucide-react";
import { shallowEqual, useApp, useHost, useLink, useUi } from "../../app/context";
import { Button, IconButton } from "../../design/Button";
import { Chip, Segmented } from "../../design/Chip";
import { Empty, ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { announce } from "../../design/Announcer";
import type { ControlTarget, TargetControlView } from "../../protocol/types";
import type { HostState } from "../../state/projection";
import { handBackTarget } from "../../state/handBack";
import { ContextHeader } from "../../shell/ContextHeader";
import { Screen } from "../../shell/Screen";
import {
  Terminals,
  computerUseBlocker,
  startBlocker,
  useAllPreviews,
  useComputerUseTasks,
  useControlQueries,
  type ControlQueries,
} from "./Control";
import { ConnectionNote } from "./Feedback";
import { LivePicture } from "./LivePicture";
import { ownership } from "./ownership";
import Previews from "./Previews";
import { useProjectScope } from "./projectScope";
import { useStore } from "../../state/store";
import { controlPart, type ControlPart } from "./parts";
import { PrivateStepWarning } from "./PrivateStepWarning";
import { ReadinessNote } from "./ReadinessNote";
import { canUnlockHere, inputBlocker } from "./readiness";
import { SafetyActions } from "./SafetyStrip";
import { Unlock } from "./Unlock";
import { useVisible } from "./useControl";
import { useMedia } from "./useMedia";
import "./controlRoot.css";

export function ControlRoot() {
  const queries = useControlQueries();
  return (
    <Screen className="control-screen" title="Control" header={<ControlHeader />}>
      <ControlNotices queries={queries} />
      <ControlParts queries={queries} />
    </Screen>
  );
}

function ControlHeader() {
  const { ui } = useApp();
  return <ContextHeader heading actions={<IconButton icon={Mic} label="Control by voice" onClick={() => ui.push({ screen: "voice" })} />} />;
}

/** What stands between this device and the computer, and only while it does. */
function ControlNotices({ queries }: { queries: ControlQueries }) {
  const { profile } = useApp();
  const views = useHost((s) => s.control);
  const caps = useHost((s) => s.capabilities);
  const grants = useHost((s) => s.grants);
  const locked = Object.values(views).some((v) => v.desktop_lock === "locked");
  const unlockHere = canUnlockHere(caps, grants);
  return (
    <>
      <ConnectionNote />
      {queries.error && (
        <Notice tone="warn" action={<Button onClick={queries.retry}>Try again</Button>}>
          {queries.error}
        </Notice>
      )}
      {Object.values(views).filter((v) => v.private_step).map((v) => <PrivateStepWarning key={v.target_id} targetId={v.target_id} />)}
      {locked && (
        <Notice icon={Lock} title={`${profile.name} is locked`}>
          {unlockHere
            ? "Unlock it here with its password, or at the computer, to watch and control it again."
            : "Unlock it at the computer to watch and control it again."}
          <Unlock />
        </Notice>
      )}
      {caps && <ReadinessNote what="Desktop capture" availability={caps.capture} />}
    </>
  );
}

type Tone = "neutral" | "accent" | "agent" | "warn";

/** Who has a display's input, in one line: the agent names its task. */
export function displayState(state: HostState, view: TargetControlView | undefined, deviceId: string | null, online: boolean): { text: string; tone: Tone } {
  if (!online) return { text: "Who has input is unknown until reconnected", tone: "warn" };
  if (view?.desktop_lock === "locked") return { text: "Locked", tone: "warn" };
  const info = ownership(view, deviceId, () => "Agent");
  if (info.state === "ready") return { text: "No one has input", tone: "neutral" };
  if (info.state === "paused" && view && waitingTask(state, view.target_id)) return { text: "Agent paused · you’re watching until you resume it or take over", tone: "warn" };
  if (info.state === "agent") {
    const task = state.tasks[leaseTask(state, view) ?? ""];
    return { text: task ? `Agent has control · ${task.title}` : info.title, tone: "agent" };
  }
  return { text: info.title, tone: info.tone };
}

/** The task whose paused agent waits for a display's input back, if one does. */
function waitingTask(state: HostState, targetId: string) {
  return Object.values(state.tasks).find((t) => t.target_id === targetId && !!handBackTarget(state, t));
}

/** The task whose agent holds a display's lease, if one does. */
function leaseTask(state: HostState, view: TargetControlView | undefined): string | undefined {
  const owner = view?.lease?.owner;
  return owner?.kind === "agent" ? state.runs[owner.run_id]?.task_id : undefined;
}

function useDisplayState(target: ControlTarget) {
  const { profile } = useApp();
  const online = useLink().state === "online";
  return useHost((s) => displayState(s, s.control[target.target_id], profile.deviceId, online), shallowEqual);
}

/** Where a display opens: the agent's task while a computer-use task drives
 * it, so its activity and Stop are in view, else the display itself. */
function useOpenDisplay(targetId: string) {
  const { ui } = useApp();
  const task = useComputerUseTasks().find((t) => t.target_id === targetId);
  return () => ui.push(task ? { screen: "computer_use", taskId: task.task_id } : { screen: "desktop", targetId });
}

/** What Screens says while there is no display to show. */
function NoDisplays() {
  const { profile } = useApp();
  const loaded = useHost((s) => s.host !== null);
  const caps = useHost((s) => s.capabilities);
  if (!loaded) return <p className="fine control-none">Loading displays…</p>;
  return caps?.capture.state === "available"
    ? <Empty icon={Monitor} title="No displays reported">Check desktop sharing on {profile.name}.</Empty>
    : null;
}

/** Computer-use tasks, and starting one. The task the live display already
 * names (`named`) is not listed twice. */
function ComputerUseRows({ named }: { named: string | null }) {
  const { ui } = useApp();
  const active = useVisible();
  const online = useLink().state === "online";
  const projectId = useUi((s) => s.projectId);
  const tasks = useComputerUseTasks();
  const targets = useHost((s) => s.targets);
  const views = useHost((s) => s.control);
  const caps = useHost((s) => s.capabilities);
  const grants = useHost((s) => s.grants);
  const locked = Object.values(views).some((v) => v.desktop_lock === "locked");
  const reason = computerUseBlocker(online, projectId, caps, grants);
  const unlockToStart = locked && canUnlockHere(caps, grants) && reason !== null &&
    caps?.computer_use.state === "temporarily_unavailable" &&
    startBlocker(online, projectId, grants) === null;
  const shown = useRef(active);
  shown.current = active;
  return (
    <>
      {tasks.filter((t) => t.task_id !== named).map((t) => (
        <ListRow
          key={t.task_id}
          icon={Bot}
          title={t.title}
          subtitle={`Computer use · ${targets.find((target) => target.target_id === t.target_id)?.label ?? "Target not reported"}`}
          chevron
          onClick={() => ui.push({ screen: "computer_use", taskId: t.task_id })}
        />
      ))}
      {reason === null && (
        <ListRow icon={Plus} title="Start computer use" subtitle="An agent works on a display while you watch" chevron
          onClick={() => { if (projectId) ui.push({ screen: "computer_use_review", projectId }); }} />
      )}
      {unlockToStart && (
        <div className="row control-unlock">
          <Unlock
            label="Unlock with password and start computer use"
            onUnlocked={() => { if (shown.current && projectId) ui.push({ screen: "computer_use_review", projectId }); }}
          />
        </div>
      )}
    </>
  );
}

function AppsRow() {
  const { ui } = useApp();
  const first = useHost((s) => s.targets[0]?.target_id ?? "");
  return <ListRow icon={AppWindow} title="Apps and windows" subtitle="Find, focus and open apps" chevron
    onClick={() => ui.push({ screen: "windows", targetId: first })} />;
}

function SystemRow() {
  const { ui } = useApp();
  const offered = useHost((s) => !!s.capabilities?.features.includes("system_stats_v1"));
  if (!offered) return null;
  return <ListRow icon={Activity} title="System" subtitle="Processor, memory, disks and network" chevron
    onClick={() => ui.push({ screen: "system" })} />;
}

type Part = ControlPart;

/** Screens, Terminals and Previews as segments, as a messenger splits its
 * chats; one kind of thing on screen at a time, Screens first. */
function ControlParts({ queries }: { queries: ControlQueries }) {
  const part = useStore(controlPart, (p) => p);
  const setPart = (p: Part) => controlPart.set(p);
  const previews = useAllPreviews();
  // The counts are the chosen project's, as the lists are.
  const { here } = useProjectScope();
  const terminals = useHost((s) => s.terminals.filter((t) => t.state.state !== "exited" && here(t.project_id)).length);
  const reachable = previews.filter((p) => !(p.automatic && p.reachable === false) && here(p.project_id)).length;
  return (
    <>
      <Segmented<Part>
        label="Show"
        className="control-parts"
        value={part}
        onChange={setPart}
        options={[
          { value: "screens", label: "Screens" },
          { value: "terminals", label: terminals ? `Terminals ${terminals}` : "Terminals" },
          { value: "previews", label: reachable ? `Previews ${reachable}` : "Previews" },
        ]}
      />
      {part === "screens" ? <LiveScreens />
        : part === "terminals" ? <Terminals queries={queries} compact title={null} />
        : <Previews previews={previews} compact title={null} />}
    </>
  );
}

/** The display the person most likely came for: the one someone is driving,
 * else the first. */
function pickDisplay(targets: ControlTarget[], views: Record<string, TargetControlView>): ControlTarget | undefined {
  const held = (t: ControlTarget) => {
    const v = views[t.target_id];
    return v?.input_state === "agent" ? 0 : v?.input_state === "human" ? 1 : v?.lease ? 2 : 3;
  };
  return [...targets].sort((a, b) => held(a) - held(b))[0];
}

/** One display live, a chip for each when there are several, and the doors
 * that belong with the screens under it. */
function LiveScreens() {
  const targets = useHost((s) => s.targets);
  const views = useHost((s) => s.control);
  const [chosen, setChosen] = useState<string | null>(null);
  const target = targets.find((t) => t.target_id === chosen) ?? pickDisplay(targets, views);
  // Its state line names the task driving it only while that task's agent
  // has input.
  const named = useHost((s) => {
    const view = target && s.control[target.target_id];
    return view?.input_state === "agent" ? leaseTask(s, view) ?? null : null;
  });
  return (
    <>
      {targets.length > 1 && target && (
        <div className="control-displays" role="group" aria-label="Display" data-scroll-x>
          {targets.map((t) => (
            <Chip key={t.target_id} icon={t.kind === "browser" ? Globe : Monitor} selected={t.target_id === target.target_id}
              onClick={() => setChosen(t.target_id)}>{t.label}</Chip>
          ))}
        </div>
      )}
      {target ? <LiveDisplay key={target.target_id} target={target} /> : <Section><div className="group"><NoDisplays /></div></Section>}
      <Section>
        <div className="group">
          <AppsRow />
          <SystemRow />
          <ComputerUseRows named={named} />
        </div>
      </Section>
    </>
  );
}

function LiveDisplay({ target }: { target: ControlTarget }) {
  const { ui, profile } = useApp();
  const visible = useVisible();
  const link = useLink();
  const online = link.state === "online";
  const view = useHost((s) => s.control[target.target_id]);
  const caps = useHost((s) => s.capabilities);
  const grants = useHost((s) => s.grants);
  const state = useDisplayState(target);
  const blocked = view?.desktop_lock === "locked" || !grants?.view;
  const mine = view?.lease?.state === "active" && view.lease.owner.kind === "human" && view.lease.owner.device_id === profile.deviceId;
  const canTakeOver = online && !inputBlocker(caps, target, view, grants);
  const takeOver = () => {
    if (!canTakeOver) return;
    announce("Taking over");
    ui.requestTakeover(target.target_id);
    ui.push({ screen: "desktop", targetId: target.target_id });
  };
  const driven = view?.input_state === "agent" || view?.input_state === "revoking" || view?.input_state === "unknown" ||
    (view?.input_state === "human" && !mine);
  // A paused agent waiting for its input back leads with Resume agent, whose
  // review shows the picture and offers Mark up; Take over sits beside it.
  const waiting = useHost((s) => !!waitingTask(s, target.target_id));
  return (
    <section className="control-live" aria-label={target.label}>
      <div className="control-live__picture">
        {blocked ? <p className="fine control-live__blocked">{view?.desktop_lock === "locked" ? "Locked · no picture" : "Watching isn’t allowed on this device"}</p>
          : <LiveDisplayPicture target={target} watch={visible && online} />}
      </div>
      <p className={`control-live__state control-state control-state--${state.tone}`}>{state.text}</p>
      {/* A display whose input can't be taken says so in its state line, not
          with a button that can never work. */}
      {!mine && (driven || waiting ? (
        <SafetyActions target={target} canTakeOver={canTakeOver} onTakeOver={takeOver} />
      ) : view?.input_state !== "unavailable" && (
        <Button block icon={Hand} disabled={!canTakeOver} onClick={takeOver}>Take over</Button>
      ))}
    </section>
  );
}

/** The picture on its own, so a frame arriving redraws only it. */
function LiveDisplayPicture({ target, watch }: { target: ControlTarget; watch: boolean }) {
  const open = useOpenDisplay(target.target_id);
  const media = useMedia(target, watch);
  return (
    <>
      <LivePicture compact target={target} canvas={media.canvas} backdrop={media.backdrop} onRegion={media.setRegion} picture={media.picture} retry={media.retry} />
      {media.picture.error === null && (
        <button type="button" className="control-live__open" aria-label={`Watch ${target.label}`} onClick={open} />
      )}
    </>
  );
}
