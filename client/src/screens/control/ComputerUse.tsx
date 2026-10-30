// SPDX-License-Identifier: Apache-2.0
import { taskActivity, taskStatus } from "../work/taskActivity";
import { useEffect, useRef, useState } from "react";
import { Shield, EyeOff, Hourglass, Lock } from "lucide-react";
import { useApp, useCommand, useHost, useLink, describeError } from "../../app/context";
import { commandResult } from "../../host/commandResult";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { Empty, Section } from "../../design/ListRow";
import { CapabilityNote, Notice } from "../../design/Notice";
import { OwnershipStrip } from "../../design/Ownership";
import { ownership } from "./ownership";
import { announce } from "../../design/Announcer";
import { Screen } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { LivePicture } from "./LivePicture";
import { useMedia } from "./useMedia";
import { useControl, useVisible } from "./useControl";
import { SafetyActions } from "./SafetyStrip";
import {
  CommandFeedback,
  ConnectionNote,
  InterventionDetails,
} from "./Feedback";
import { desktopBlocker, inputBlocker } from "./readiness";
import { Unlock } from "./Unlock";
import { usesComputerItself } from "./computerReview";
import { displayModel, earlierHelpers } from "../work/logic";
import type { ObservationState } from "../../protocol/types";
import { handBackTarget } from "../../state/handBack";
import "./control.css";

const observationWords: Record<ObservationState, string> = {
  allowed: "The agent can receive new pictures of this display.",
  withdrawal_pending: "Stopping new pictures to the agent. The computer has not confirmed yet.",
  withdrawn: "New pictures to the agent have stopped. Earlier pictures remain in its conversation.",
  unknown: "The computer has not confirmed whether the agent can receive new pictures.",
};

/** A paused agent's display, in one line: watching, the agent, or you. */
export const PAUSED_CHOICE = "You’re watching. Resume the agent, or take over to use the screen yourself.";

export default function ComputerUse({ route }: RouteProps<"computer_use">) {
  const { profile, ui, session } = useApp();
  const task = useHost((s) => s.tasks[route.taskId]);
  const runInfo = useHost((s) =>
    task?.current_run ? s.runs[task.current_run] : undefined,
  );
  const status = useHost(s => task ? taskStatus(task, runInfo, s.control[task.target_id ?? ""]) : null);
  const activity = useHost((s) => taskActivity(s, task));
  const target = useHost((s) =>
    s.targets.find((t) => t.target_id === task?.target_id),
  );
  const caps = useHost((s) => s.capabilities);
  const harness = useHost((s) =>
    s.harnesses.find((h) => h.kind === task?.harness),
  );
  const grants = useHost((s) => s.grants);
  const visible = useVisible();
  const link = useLink();
  const online = link.state === "online";
  const accessUnavailable = link.state === "failed";
  // The footer's Resume agent, with its picture and Mark up, already offers it.
  const footerResumes = useHost((s) => !!target && Object.values(s.tasks).some((t) => t.target_id === target.target_id && !!handBackTarget(s, t)));
  const lockReason = useHost((s) => desktopBlocker(s.control[target?.target_id ?? ""], s.capabilities, s.grants));
  const media = useMedia(target, visible && !accessUnavailable && !!grants?.view && !lockReason);
  const control = useControl(target?.target_id, media.latest, visible);
  const command = useCommand();
  const [review, setReview] = useState(false);
  const [resuming, setResuming] = useState(false);
  const [resumeError, setResumeError] = useState<string | null>(null);
  const inputReason = inputBlocker(caps, target, control.view, grants);
  const privateStep = control.view?.private_step;
  const privateReview = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (visible && privateStep) {
      privateReview.current?.focus({ preventScroll: true });
      privateReview.current?.scrollIntoView?.({ block: "start" });
    }
  }, [visible, !!privateStep]);
  const ownerInfo = ownership(control.view, profile.deviceId, () => "Agent");
  // The rail already says the agent has input; the strip shows only when
  // something is out of the ordinary.
  const ordinary = ownerInfo.state === "agent" && !lockReason && !control.leaving;
  // Paused for the footer's Resume agent: what each of its actions means.
  if (ownerInfo.state === "paused" && footerResumes) ownerInfo.detail = PAUSED_CHOICE;
  if (lockReason) {
    ownerInfo.title = "Picture and input paused";
    ownerInfo.detail = lockReason;
    ownerInfo.icon = Lock;
    ownerInfo.tone = "neutral";
  }
  if (control.leaving && !lockReason) {
    ownerInfo.title = control.leaving === "pending" ? "Leaving control…" : "Leave unconfirmed";
    ownerInfo.detail = "Input from this device is off. Waiting for the computer to confirm release.";
    ownerInfo.icon = Hourglass;
    ownerInfo.tone = "neutral";
  }
  // This view stays read-only, so taking over continues on Desktop. Leaving
  // this view pauses the agent's input at once; Desktop then takes control.
  const take = () => {
    if (!target || inputBlocker(session.state.get().capabilities, target, session.state.get().control[target.target_id], session.state.get().grants)) return;
    announce("Taking over");
    ui.requestTakeover(target.target_id);
    ui.push({ screen: "desktop", targetId: target.target_id });
  };
  const resumable =
    runInfo &&
    ["working", "awaiting_approval", "awaiting_input"].includes(
      runInfo.state,
    ) &&
    ["paused", "ready", "human"].includes(control.view?.input_state ?? "") && !privateStep && !footerResumes;
  const footer = task && (
    <div className="control-footer">
      {inputReason && <Notice>{inputReason}</Notice>}
      <SafetyActions
        target={target}
        human={control.mine && !accessUnavailable}
        onLeave={control.leave}
        leaving={control.leaving}
        canTakeOver={
          !inputReason && control.reconciled &&
          control.isFresh &&
          target?.input.state === "available" && (!privateStep || privateStep.ready)
        }
        onTakeOver={take}
      />
    </div>
  );
  return (
    <Screen
      title="Computer use"
      bodyHeading={!!task}
      subtitle={target ? `${target.label} · ${profile.name}` : profile.name}
      className="control-screen agent-screen"
      footer={footer}
    >
      {!task ? (
        <Empty title="Task unavailable">
          This task is not in the computer’s current state. Return to Control to
          review its tasks.
        </Empty>
      ) : (
        <>
          <ConnectionNote />
          <CommandFeedback targetId={target?.target_id} taskId={task.task_id} />
          <OwnershipStrip info={ownerInfo} className={ordinary ? "visually-hidden" : undefined} />
          <div className="control-task">
            <h1>{task.title}</h1>
            <p className="control-task__activity">
              {activity?.text ??
                status ??
                "No current action reported"}
            </p>
            <p className="fine">
              Reported{" "}
              <Age at={activity?.at ?? task.updated_at} suffix=" ago" />
            </p>
          </div>
          {accessUnavailable ? (
            <Empty title="Picture unavailable">Access to the computer is unavailable.</Empty>
          ) : lockReason ? (
            <>
              <Empty icon={Lock} title={control.view?.desktop_lock === "locked" ? "Picture paused while locked" : "Picture unavailable"}>{lockReason}</Empty>
              {control.view?.desktop_lock === "locked" && <Unlock />}
            </>
          ) : target ? (
            <LivePicture
              target={target}
              canvas={media.canvas} backdrop={media.backdrop} onRegion={media.setRegion}
              picture={media.picture}
              retry={media.retry}
              leaving={control.leaving}
            />
          ) : (
            <Notice tone="unknown">
              The computer has not reported a current display for this task.
            </Notice>
          )}
          {resumeError && <Notice tone="unknown">{resumeError}</Notice>}
          {control.error && <Notice tone="warn">{control.error}</Notice>}
          {caps && (
            <CapabilityNote
              what="Managed computer use"
              availability={caps.computer_use}
            />
          )}
          {harness && (
            <CapabilityNote
              what="Agent delegation"
              availability={harness.capabilities.computer_use_delegation}
            />
          )}
          {task.state === "failed" && (
            <Notice tone="stop" title="The desktop task failed">
              {status ??
                "The agent did not report a reason. Review the task receipt for available evidence."}
            </Notice>
          )}
          <Section title="Agent and model" id="computer-agent">
            <dl className="control-details">
              <div>
                <dt>Agent</dt>
                <dd>{harness?.display_name ?? task.harness}</dd>
              </div>
              <div>
                <dt>Model in use</dt>
                <dd>{displayModel(runInfo?.actual_model ?? null, harness, "Not reported by the agent")}</dd>
              </div>
              <div>
                <dt>Helper agent</dt>
                <dd>
                  {runInfo?.children.length
                    ? runInfo.children.map((c) => (
                        <p key={c.child_id}>
                          {c.label} · {c.state}
                        </p>
                      ))
                    : usesComputerItself(task.harness)
                      ? `None: ${harness?.display_name ?? "the agent"} uses the computer itself, with only the computer tools.`
                      : "Not known. The agent hasn’t said whether a helper agent works the desktop for it."}
                  {!!runInfo?.earlier_children && <p>{earlierHelpers(runInfo.earlier_children)}</p>}
                </dd>
              </div>
              <div>
                <dt>Pictures for the agent</dt>
                <dd>
                  {observationWords[control.view?.model_observation?.state ?? "unknown"]}
                </dd>
              </div>
            </dl>
          </Section>
          {!accessUnavailable && !control.leaving && <InterventionDetails
            targetId={target?.target_id}
            taskId={task.task_id}
          />}
          {resumable && target && (
            <Section title="Resume the agent" id="return-agent">
              <p className="fine">
                Review the current picture and remove anything sensitive before
                you resume the agent.
              </p>
              {review ? (
                <div className="control-review">
                  <p>
                    Allow this run to use <strong>{target.label}</strong> again?
                    It will receive a new picture.
                  </p>
                  <Button
                    variant="primary"
                    disabled={
                      !online || !!inputReason ||
                      !control.reconciled ||
                      !control.isFresh ||
                      !!privateStep
                    }
                    onClick={() => {
                      control.leave();
                      void command(
                        {
                          command: "return_to_agent",
                          target_id: target.target_id,
                          run_id: runInfo.run_id,
                        },
                        { label: "Resume agent requested" },
                      );
                      setReview(false);
                    }}
                  >
                    Resume now
                  </Button>
                  <Button variant="quiet" onClick={() => setReview(false)}>
                    Keep agent paused
                  </Button>
                </div>
              ) : (
                <Button disabled={!!inputReason} onClick={() => setReview(true)}>
                  Resume agent
                </Button>
              )}
            </Section>
          )}
          {target && (
            <div ref={privateReview} tabIndex={-1}><Section title="Private human step" id="private-step">
              <p className="fine">
                Stops agent input and new screenshots, then pauses the agent’s session. Wait for confirmation before entering anything private.
              </p>
              {privateStep && (
                <Notice
                  tone={
                    privateStep.ready &&
                    privateStep.uncovered_routes.length === 0
                      ? "info"
                      : "unknown"
                  }
                  title={
                    privateStep.ready &&
                    privateStep.uncovered_routes.length === 0
                      ? "Private step ready"
                      : "Privacy is not confirmed"
                  }
                >
                  <p>{privateStep.ready ? "Agent input stopped. Screenshots to the agent stopped. The agent’s session is paused. Earlier pictures stay in the conversation." : "The computer hasn’t confirmed every part of the private step. Earlier pictures stay in the conversation."}</p>
                  {privateStep.uncovered_routes.map((r) => (
                    <p key={r}>{r}</p>
                  ))}
                </Notice>
              )}
              <div className="control-keys">
                <Button
                  icon={Shield}
                  disabled={!online}
                  onClick={() =>
                    void command(
                      {
                        command: privateStep
                          ? "end_private_step"
                          : "start_private_step",
                        target_id: target.target_id,
                      },
                      {
                        label: privateStep
                          ? "End private step requested"
                          : "Private step requested",
                      },
                    )
                  }
                >
                  {privateStep ? "End private step" : "Start private step"}
                </Button>
                {runInfo && (
                  <Button
                    icon={EyeOff}
                    disabled={
                      !online ||
                      control.view?.model_observation?.state !== "allowed"
                    }
                    onClick={() =>
                      void command(
                        {
                          command: "withdraw_observation",
                          run_id: runInfo.run_id,
                          target_id: target.target_id,
                        },
                        { label: "Stop new pictures to the agent requested" },
                      )
                    }
                  >
                    Stop pictures to agent
                  </Button>
                )}
              </div>
              <p className="fine">Ending the private step keeps the agent paused.</p>
              {runInfo?.native_session && task.state === "interrupted" && <div className="control-review">
                <p>Remove anything sensitive and review the current picture before resuming. The agent must look at a fresh picture before using input again.</p>
                <Button variant="primary" pending={resuming} disabled={!online || !!inputReason || !control.isFresh || !control.reconciled || !!privateStep}
                  onClick={async () => {
                    setResuming(true); setResumeError(null);
                    try {
                      const receipt = await session.command({ command: "resume_task", task_id: task.task_id, text: "The private human step is finished. Observe the current target before continuing the reviewed goal. Do not replay any queued actions." }, { label: "Resume computer use", expectedRevision: task.revision });
                      await commandResult(session, receipt);
                    } catch (e) { setResumeError(describeError(e, profile.name)); }
                    finally { setResuming(false); }
                  }}>Resume computer use</Button>
              </div>}
            </Section></div>
          )}
        </>
      )}
    </Screen>
  );
}
