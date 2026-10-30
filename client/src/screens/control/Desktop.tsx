// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState, type MouseEvent, type PointerEvent as ReactPointerEvent } from "react";
import { AppWindow, Hand, Hourglass, Keyboard as KeyboardIcon, Lock, Mic, MoreHorizontal, Pencil, Square } from "lucide-react";
import { useImmersive } from "../../shell/focus";
import { describeError, useApp, useCommand, useHost, useLink } from "../../app/context";
import { commandResult } from "../../host/commandResult";
import { TAKEOVER_MS } from "../../app/ui";
import { Button, IconButton } from "../../design/Button";
import { CapabilityNote, Notice } from "../../design/Notice";
import { Empty, ListRow } from "../../design/ListRow";
import { TextArea } from "../../design/Field";
import { Sheet } from "../../design/Sheet";
import { StateLine } from "../../design/StateLine";
import { useStore } from "../../state/store";
import { prefs } from "../../app/prefs";
import { OwnershipStrip } from "../../design/Ownership";
import { controlStateKnown, ownership } from "./ownership";
import { announce } from "../../design/Announcer";
import { NotAllowed } from "../../shell/NotAllowed";
import { Screen, useRoute } from "../../shell/Screen";
import type { RouteProps } from "../types";
import { useMedia } from "./useMedia";
import { useControl, useVisible } from "./useControl";
import { LivePicture } from "./LivePicture";
import { Keyboard } from "./Keyboard";
import { SafetyActions } from "./SafetyStrip";
import {
  CommandFeedback,
  ConnectionNote,
  InterventionDetails,
} from "./Feedback";
import { desktopBlocker, inputBlocker } from "./readiness";
import { PrivateStepWarning } from "./PrivateStepWarning";
import { RenameScreen } from "./RenameScreen";
import { handBackTarget } from "../../state/handBack";
import { Remedy } from "./ReadinessNote";
import { SignInStrip } from "./SignInStrip";
import { Unlock } from "./Unlock";
import { FullControl, useLandscape } from "./FullControl";
import { VoiceDisclosure } from "./Voice";
import { VoiceEnded, VoicePrompt, VoiceSteps } from "./VoiceSession";
import { useVoice } from "./VoiceState";
import { useVoiceSession } from "./useVoiceSession";
import { dropVoice, startVoice, useVoiceHold } from "./voiceHold";
import "./control.css";

export default function Desktop({ route }: RouteProps<"desktop">) {
  const { profile, session, ui } = useApp();
  const target = useHost((s) =>
    s.targets.find((t) => t.target_id === route.targetId),
  );
  const grants = useHost((s) => s.grants);
  const capabilities = useHost((s) => s.capabilities);
  const visible = useVisible();
  const link = useLink();
  const online = link.state === "online";
  const caughtUp = useHost(s => s.caughtUp);
  const accessUnavailable = link.state === "failed";
  const body = useRef<HTMLDivElement>(null);
  useEffect(() => { if (accessUnavailable && body.current) body.current.scrollTop = 0; }, [accessUnavailable]);
  const lockReason = useHost((s) => desktopBlocker(s.control[route.targetId], s.capabilities, s.grants));
  const locked = !!lockReason;
  const confirmedLocked = useHost((s) => s.control[route.targetId]?.desktop_lock === "locked");
  const displayChanged = target?.capture.state === "temporarily_unavailable" && target.capture.reason.includes("Display changed");
  const media = useMedia(target, visible && !accessUnavailable && !!grants?.view && !locked);
  const control = useControl(route.targetId, media.latest, visible);
  const task = useHost((s) => {
    const owner = s.control[route.targetId]?.lease?.owner;
    return owner?.kind === "agent"
      ? s.tasks[s.runs[owner.run_id]?.task_id ?? ""]
      : undefined;
  });
  const waiting = useHost((s) => Object.values(s.tasks).some((t) => t.target_id === route.targetId && !!handBackTarget(s, t)));
  const run = useCommand();
  const [keyboard, setKeyboard] = useState(false);
  const [requesting, setRequesting] = useState(false);
  const [requestError, setRequestError] = useState<string | null>(null);
  // Take over asked for on AI Computer Use is finished here, where input works.
  const [handoff, setHandoff] = useState(() =>
    ui.takeoverRequested(route.targetId),
  );
  const [cameForTakeover] = useState(handoff);
  // Voice control of this display: shown here from start to an ended notice
  // that stays until closed.
  const hold = useVoiceHold(session, profile.profileId, route.targetId);
  const [voiceId, setVoiceId] = useState<string | null>(null);
  useEffect(() => { if (hold?.sessionId) setVoiceId(hold.sessionId); }, [hold?.sessionId]);
  const voice = useVoiceSession(voiceId ?? undefined, visible && !accessUnavailable);
  const voiceOn = !!hold && !voice.value?.ended;
  // A session the computer does not know is no longer this display's.
  useEffect(() => {
    if (!voiceId || !voice.view || voice.view.session) return;
    dropVoice(session, profile.profileId, route.targetId, voiceId);
    setVoiceId(null);
  }, [session, profile.profileId, route.targetId, voiceId, voice.view]);
  const [voiceSheet, setVoiceSheet] = useState(false);
  const voiceCheck = useVoice(undefined, voiceSheet && visible);
  const [dryRun, setDryRun] = useState(true);
  const [voiceStarting, setVoiceStarting] = useState(false);
  const [voiceError, setVoiceError] = useState<string | null>(null);
  const acquire = async (takeover: boolean) => {
    const state = session.state.get();
    if (!session.online || !state.grants?.view || !state.grants.control_input || !target || inputBlocker(state.capabilities, target, state.control[target.target_id], state.grants)) return;
    control.arm();
    setRequesting(true);
    setRequestError(null);
    announce("Taking over");
    try {
      const receipt = await session.command(
        {
          command: takeover ? "take_over" : "request_human_lease",
          target_id: target.target_id,
          geometry_revision: target.geometry.revision,
        },
        { label: "Taking over…" },
      );
      const result = await commandResult(session, receipt);
      if (result?.kind !== "lease_granted") throw new Error("The computer hasn’t confirmed control. Check its current state before trying again.");
      control.settled(result.lease);
    } catch (error) {
      control.leave();
      const message = describeError(error, profile.name);
      setRequestError(message);
      announce(message);
    } finally { setRequesting(false); }
  };
  const startHere = async () => {
    if (!target) return;
    setVoiceStarting(true);
    setVoiceError(null);
    setKeyboard(false);
    try {
      const started = await startVoice(session, profile.profileId, target, "desktop", dryRun);
      if (started) {
        setVoiceId(started.session_id);
        setVoiceSheet(false);
        announce(started.dry_run ? "Voice preview started" : "Voice control started");
      } else setVoiceError("The computer hasn’t confirmed a voice session. Check Control before starting again.");
    } catch (e) {
      setVoiceError(describeError(e, profile.name));
    } finally { setVoiceStarting(false); }
  };
  /** Sensitive fields are typed, not spoken: hand input back to this device. */
  const keyboardInstead = async () => {
    if (!(await voice.stop())) return;
    setVoiceId(null);
    await acquire(false);
    setKeyboard(true);
  };
  const owner = control.view;
  const ownerInfo = ownership(owner, profile.deviceId, () => "Agent");
  if (locked) {
    ownerInfo.title = confirmedLocked ? "Locked on computer" : "Desktop state unknown";
    ownerInfo.detail = lockReason ?? "";
    ownerInfo.icon = Lock;
    ownerInfo.tone = "neutral";
  }
  if (!locked && control.mine && !control.enabled)
    ownerInfo.detail =
      "Input from this device is paused until the picture and connection are current.";
  if (!locked && control.leaving) {
    ownerInfo.title = control.leaving === "pending" ? "Leaving control…" : "Leave unconfirmed";
    ownerInfo.detail = "Input from this device is off. Waiting for the computer to confirm release.";
    ownerInfo.icon = Hourglass;
    ownerInfo.tone = "neutral";
  }
  if (!locked && voiceOn && !control.leaving) {
    ownerInfo.title = voice.value?.dry_run ? "Voice preview" : "Voice control";
    ownerInfo.detail = "Touch input from this device is off. Stop voice to use touch or the keyboard again.";
    ownerInfo.icon = Mic;
    ownerInfo.tone = "accent";
  }
  if (!controlStateKnown(online, caughtUp)) {
    ownerInfo.title = "Control state unknown on this device";
    ownerInfo.detail = "Waiting for the computer to confirm who holds input.";
    ownerInfo.tone = "neutral";
  }
  const inputReason = inputBlocker(capabilities, target, owner, grants);
  // The bar says who has input; when there is more to say than that, the
  // strip says it instead, once. The rest waits under More.
  const [more, setMore] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const notable = locked || !!control.leaving || !controlStateKnown(online, caughtUp) ||
    (control.mine && !control.enabled) || ["unknown", "revoking"].includes(owner?.input_state ?? "unknown");
  const canAcquire =
    online &&
    !inputReason &&
    control.reconciled &&
    control.isFresh &&
    target?.input.state === "available" &&
    !!grants?.control_input && !!grants.view;
  useEffect(() => {
    if (!handoff) return;
    const expire = setTimeout(() => setHandoff(false), TAKEOVER_MS);
    return () => clearTimeout(expire);
  }, [handoff]);
  useEffect(() => {
    // Wait for a pause already under way, then take what the buttons offer.
    if (!handoff || !canAcquire || requesting) return;
    const state = owner?.input_state;
    if (state === "revoking") return;
    setHandoff(false);
    const offered =
      state !== "unknown" && (!!task || state === "ready" || state === "paused");
    if (ui.claimTakeover(route.targetId) && offered) void acquire(!!task);
  });
  const leave = () => {
    setKeyboard(false);
    control.leave();
    // A detour made only to take over ends where it began.
    if (cameForTakeover) ui.finish(["desktop"]);
  };
  // Hold to talk; a keyboard or switch presses it once to start and again to finish.
  const talk = {
    "aria-label": voice.listening ? "Release to finish speaking" : "Hold to talk",
    "aria-pressed": voice.listening,
    disabled: !voice.usable || voice.view?.speech.state !== "available",
    onPointerDown: (e: ReactPointerEvent<HTMLButtonElement>) => { e.currentTarget.setPointerCapture(e.pointerId); voice.startSpeech(); },
    onPointerUp: voice.finishSpeech,
    onPointerCancel: voice.closeSpeech,
    onClick: (e: MouseEvent<HTMLButtonElement>) => { if (e.detail === 0) { if (voice.listening) voice.finishSpeech(); else voice.startSpeech(); } },
  };
  const voiceStop = (
    <Button
      variant="stop"
      icon={Square}
      className="intervention"
      pending={voice.stopping}
      disabled={!online || !voiceId}
      onClick={() => void voice.stop()}
    >
      Stop voice
    </Button>
  );
  const footer = target && !accessUnavailable && (
    <div className="control-footer">
      {requestError && !accessUnavailable && <Notice tone="unknown">{requestError}</Notice>}
      {inputReason && !locked && <p className="fine" id="control-input-reason"><Remedy text={inputReason} /></p>}
      {!control.mine && (
        <CommandFeedback targetId={target.target_id} taskId={task?.task_id} />
      )}
      {locked ? (
        <Notice icon={Lock}>{confirmedLocked ? "Locked · input off" : "Desktop state unknown · input off"}</Notice>
      ) : voiceOn ? (
        <div className="desktop-voice-actions">
          <Button variant="primary" icon={Mic} {...talk}>
            {voice.listening ? "Listening…" : "Hold to talk"}
          </Button>
          {voiceStop}
        </div>
      ) : control.mine ? (
        <SafetyActions
          target={target}
          human
          onLeave={leave}
          leaving={control.leaving}
        />
      ) : task || waiting || owner?.input_state === "unknown" ? (
        <SafetyActions
          target={target}
          canTakeOver={
            canAcquire && !requesting && owner?.input_state !== "unknown"
          }
          takingOver={requesting || handoff}
          onTakeOver={task || waiting ? () => void acquire(!!task) : undefined}
          compact={!task}
        />
      ) : (
        <Button
          block
          variant="primary"
          icon={Hand}
          aria-describedby={inputReason ? "control-input-reason" : undefined}
          pending={requesting || handoff}
          disabled={
            !canAcquire ||
            (owner?.input_state !== "ready" && owner?.input_state !== "paused")
          }
          onClick={() => void acquire(false)}
        >
          {handoff
            ? "Taking over…"
            : requesting
              ? "Taking over…"
              : "Take over"}
        </Button>
      )}
    </div>
  );
  const textFeedback = <>
          {control.error && <Notice tone="warn">{control.error}</Notice>}
          {control.savedText && (
            <section aria-label="Saved text" className="control-lock">
              <p>
                {control.textPending
                  ? "Text sent · waiting for the computer"
                  : "Text kept on this device"}
              </p>
              <p className="fine control-saved-text">{control.savedText}</p>
              {control.pasteOffered && !locked && (
                <>
                  <p className="fine">
                    Paste replaces the computer’s clipboard and sends Ctrl+V
                    to the focused app.
                  </p>
                  <Button
                    disabled={!control.enabled || control.textPending}
                    onClick={() => control.send({ action: "paste", text: control.savedText })}
                  >
                    Paste instead
                  </Button>
                </>
              )}
              <Button variant="quiet" disabled={control.textPending} onClick={control.discardText}>
                Discard saved text
              </Button>
            </section>
          )}
  </>;
  const typingFeedback = control.pasteOffered ? <section aria-label="Saved text" className="typing-recovery">
    <p className="fine" role="status">Nothing was typed. Your text is kept here.</p>
    <Button disabled={!control.enabled || control.textPending} onClick={() => control.send({ action: "paste", text: control.savedText })}>Paste instead</Button>
    <p className="fine">Replaces the computer’s clipboard and sends Ctrl+V to the focused app.</p>
    <details><summary>Why typing was refused</summary><p className="fine">{control.error}</p></details>
  </section> : control.error ? <Notice tone="warn">{control.error}</Notice> : null;
  const typing = keyboard && control.mine && visible && !locked && !accessUnavailable && !control.leaving;
  // While this device controls a display, or with the phone on its side, the
  // picture fills the screen and its controls lie over it (FullControl.tsx).
  const tilt = useStore(prefs, (p) => p.tiltPan === true);
  const landscape = useLandscape();
  const { active } = useRoute();
  const full = !!target && !accessUnavailable && !locked && !displayChanged &&
    target.capture.state !== "revoked" && grants?.view !== false && !typing && (control.mine || landscape);
  useImmersive(full && active);
  const actualNote = !locked && !accessUnavailable && <p className="fine">
      This is the computer’s actual desktop. Someone at the computer can
      also move the pointer and type.
    </p>;
  const lockControls = !accessUnavailable && !locked && <div className="control-lock">
      <Button
        icon={Lock}
        disabled={
          !online || capabilities?.lock_session.state !== "available"
        }
        onClick={() => {
          setMore(false);
          control.leave();
          announce("Lock requested");
          void run(
            { command: "lock_desktop" },
            { label: "Lock requested" },
          );
        }}
      >
        Lock desktop
      </Button>
      <p className="fine">
        {control.mine
          ? "Leave control stops input from this device. Lock desktop also requests the computer’s lock screen."
          : "Lock desktop requests the computer’s lock screen."}
      </p>
    </div>;

  const voiceOffered = !!target && !typing && !voiceOn && !accessUnavailable && !locked
    && !!grants?.view && !!grants.control_input && target.capture.state !== "revoked";
  const sessionError = voice.error ?? voice.queryError;
  const openVoice = () => { setVoiceError(null); setVoiceSheet(true); };
  // Full screen, the overlay keeps to what needs an answer.
  const voicePanel = (brief: boolean) => voiceId && voice.value && (
    <section aria-label="Voice control" className="desktop-voice voice-stack">
      {voice.value.ended ? (
        <VoiceEnded value={voice.value} action={<Button onClick={() => setVoiceId(null)}>Close</Button>} />
      ) : <>
        {voice.value.dry_run && !brief && <Notice>Preview only. The computer will interpret commands without sending input.</Notice>}
        <VoicePrompt value={voice.value} disabled={!voice.usable || voice.busy} send={(o) => void voice.send(o)} />
        {voice.value.phase === "refused" && (
          <Button icon={KeyboardIcon} disabled={!online} pending={voice.stopping || requesting} onClick={() => void keyboardInstead()}>
            Stop voice and use keyboard
          </Button>
        )}
        {(voice.heard || voice.value.transcript) && <p className="voice-transcript" aria-label="Live transcript">{voice.heard || voice.value.transcript}</p>}
        {!brief && <>
          {voice.view && <CapabilityNote what="Speech" availability={voice.view.speech} action={<Button onClick={() => ui.push({ screen: "settings", section: "settings-voice" })}>Set up local speech</Button>} />}
          <TextArea label="Typed command" value={voice.text} onChange={voice.save} placeholder="Click Sign in, or type “hello”" rows={2} />
          <Button variant="primary" disabled={!voice.usable || !voice.text.trim()} pending={voice.busy} onClick={() => void voice.sendTyped()}>Send command</Button>
          {voice.value.steps.length > 0 && <VoiceSteps value={voice.value} last={3} />}
        </>}
      </>}
      {sessionError && <Notice tone="unknown">{sessionError}</Notice>}
    </section>
  );
  const voiceStart = target && (
        <Sheet
          open={voiceSheet}
          onClose={() => !voiceStarting && setVoiceSheet(false)}
          title="Start voice control"
          footer={
            <Button block variant="primary" pending={voiceStarting} disabled={!online || voiceCheck.view?.voice_desktop.state !== "available"} onClick={() => void startHere()}>
              {dryRun ? "Accept and preview commands" : "Accept and take over"}
            </Button>
          }
        >
          <div className="voice-stack">
            <p>Say what to do in the app in focus on {target.label}. A typed command works too.</p>
            {voiceCheck.error && <Notice tone="unknown" action={<Button disabled={!online} onClick={voiceCheck.reload}>Check again</Button>}>{voiceCheck.error}</Notice>}
            {voiceCheck.view ? <>
              <CapabilityNote what="Desktop voice control" availability={voiceCheck.view.voice_desktop} />
              <CapabilityNote what="Speech" availability={voiceCheck.view.speech} action={<Button onClick={() => { setVoiceSheet(false); ui.push({ screen: "settings", section: "settings-voice" }); }}>Set up local speech</Button>} />
            </> : !voiceCheck.error && <p className="fine">Checking voice control…</p>}
            <Notice title="Take over computer control">
              Starting ends the current managed input on {target.label}, including touch input from this device. Anyone at the computer can still use it and stop voice control.
            </Notice>
            <label className="voice-check"><input type="checkbox" checked={dryRun} onChange={(e) => setDryRun(e.target.checked)} /><span>Preview only · send no input</span></label>
            <VoiceDisclosure />
            {voiceError && <Notice tone="unknown">{voiceError}</Notice>}
            {voiceStarting && <StateLine phase="sending" />}
          </div>
        </Sheet>
  );
  if (full && target)
    return (
      <Screen bare fixed title={target.label} className="control-screen desktop-screen desktop-screen--full">
        <LivePicture
          full
          tilt={tilt}
          voice={voiceOn}
          target={target}
          canvas={media.canvas}
          backdrop={media.backdrop}
          onRegion={media.setRegion}
          picture={media.picture}
          enabled={control.enabled}
          leaving={control.leaving}
          send={control.send}
          retry={media.retry}
          overlay={(tools) => <FullControl
            tools={tools}
            title={target.label}
            owner={ownerInfo}
            task={task}
            canInput={control.mine}
            safety={voiceOn ? voiceStop : control.mine ? (
              <SafetyActions target={target} human onLeave={leave} leaving={control.leaving} />
            ) : task || waiting || owner?.input_state === "unknown" ? (
              <SafetyActions
                target={target}
                canTakeOver={canAcquire && !requesting && owner?.input_state !== "unknown"}
                takingOver={requesting || handoff}
                onTakeOver={task || waiting ? () => void acquire(!!task) : undefined}
                compact={!task}
              />
            ) : (
              <Button
                className="intervention"
                variant="primary"
                icon={Hand}
                pending={requesting || handoff}
                disabled={!canAcquire || (owner?.input_state !== "ready" && owner?.input_state !== "paused")}
                onClick={() => void acquire(false)}
              >
                {requesting || handoff ? "Taking over…" : "Take over"}
              </Button>
            )}
            onKeyboard={control.mine && !control.leaving ? () => setKeyboard(true) : undefined}
            onJev={voiceOffered ? openVoice : undefined}
            voice={voiceOn && <button type="button" className={`full-tool${voice.listening ? " is-pressed" : ""}`} {...talk}>
              <Mic aria-hidden /><span className="full-tool__label" aria-hidden>{voice.listening ? "Listening" : "Talk"}</span>
            </button>}
            tilt={tilt}
            onTilt={() => prefs.set((p) => ({ ...p, tiltPan: !p.tiltPan }))}
            notices={<>
              <ConnectionNote />
              {voicePanel(true)}
              {requestError && <Notice tone="unknown">{requestError}</Notice>}
              {inputReason && !control.mine && <p className="fine full-reason"><Remedy text={inputReason} /></p>}
              {!control.mine && <CommandFeedback targetId={target.target_id} taskId={task?.task_id} />}
              {control.error && <Notice tone="warn">{control.error}</Notice>}
              <PrivateStepWarning targetId={target.target_id} />
            </>}
          />}
        />
        {voiceStart}
      </Screen>
    );
  return (
    <Screen
      title={target?.label ?? "Desktop"}
      subtitle={typing || notable ? undefined : ownerInfo.title}
      className={`control-screen desktop-screen${typing ? " desktop-screen--typing" : ""}`}
      fixed={!!typing}
      actions={typing ? <Button size="sm" variant="warn" onClick={leave}>Leave control</Button>
        : target && !accessUnavailable ? <>
          {/* An icon, so a display's name keeps the room to stay on one line. */}
          {voiceOffered && <IconButton icon={Mic} label="Voice" disabled={!online} onClick={openVoice} />}
          <IconButton icon={MoreHorizontal} label="More" onClick={() => setMore(true)} />
        </> : undefined}
      footer={typing ? undefined : footer}
      bodyRef={body}
    >
      {typing && target ? (
        <div className="desktop-typing">
          <SignInStrip send={control.send} canType={control.mine && control.enabled} />
          <LivePicture compact target={target} canvas={media.canvas} backdrop={media.backdrop} onRegion={media.setRegion} picture={media.picture} retry={media.retry} />
          <div className="desktop-typing__composer">
            <p className="fine desktop-typing__status" role="status">
              {!online || !control.reconciled ? "Input state unknown · input from this device is off"
                : control.enabled ? "This device holds input"
                : "This device holds input · sending paused"}
            </p>
            <Keyboard targetId={target.target_id} enabled={control.enabled} send={control.send} onClose={() => setKeyboard(false)} feedback={typingFeedback} pasteOffered={control.pasteOffered} onEdit={control.editText} confirmedText={control.confirmedText} pending={control.textPending} />
          </div>
        </div>
      ) : !target ? (
        <Empty title="Display unavailable">
          The computer no longer lists this display. Return to Control to choose
          a current target.
        </Empty>
      ) : (
        <>
          <ConnectionNote />
          <SignInStrip send={control.send} canType={control.mine && control.enabled} />
          <OwnershipStrip info={ownerInfo} className={notable ? undefined : "visually-hidden"} />
          <PrivateStepWarning targetId={target.target_id} />
          {grants?.view === false && (
            <NotAllowed what="watch the desktop" permission="view" />
          )}
          {displayChanged ? (
            <Notice
              tone="warn"
              action={
                <Button disabled={!online || locked} onClick={media.reopen}>
                  Reopen sharing
                </Button>
              }
            >
              Display changed, reopen to continue. This starts a new stream;
              the computer may ask for screen-sharing permission again.
            </Notice>
          ) : target.capture.state === "revoked" ? (
            <Notice tone="warn">
              Sharing ended. Enable desktop sharing on the computer to reconnect.
            </Notice>
          ) : (
            <CapabilityNote what="Picture" availability={target.capture} />
          )}
          {accessUnavailable ? (
            <Empty title="Picture unavailable">Access to the computer is unavailable.</Empty>
          ) : locked ? (
            <>
              <Empty icon={Lock} title={confirmedLocked ? "Picture paused while locked" : "Picture unavailable"}>
                {lockReason}
              </Empty>
              {confirmedLocked && <Unlock />}
            </>
          ) : target.capture.state === "revoked" ? (
            <Empty title="Screen sharing ended">Input stays off.</Empty>
          ) : (
            <LivePicture
              target={target}
              canvas={media.canvas} backdrop={media.backdrop} onRegion={media.setRegion}
              picture={media.picture}
              enabled={control.enabled}
              leaving={control.leaving}
              voice={voiceOn}
              send={control.send}
              retry={media.retry}
            />
          )}
          {voicePanel(false)}
          {textFeedback}
          {target.input.state === "revoked" ? (
            target.capture.state !== "revoked" && (
              <Notice tone="warn">
                Input permission ended. Enable desktop sharing on the computer to
                control it again.
              </Notice>
            )
          ) : inputReason && !locked ? null : (
            // Otherwise the reason under Take over says this once.
            <CapabilityNote what="Input" availability={target.input} />
          )}
          {grants?.control_input === false && (
            <NotAllowed what="control the desktop" permission="control" />
          )}
          {control.mine && !accessUnavailable && !locked && !control.leaving && (
            <Button icon={KeyboardIcon} onClick={() => setKeyboard((v) => !v)}>
              {keyboard ? "Hide keyboard" : "Keyboard"}
            </Button>
          )}
          {keyboard && control.mine && visible && !accessUnavailable && !locked && (
            <Keyboard
              targetId={target.target_id}
              enabled={control.enabled}
              confirmedText={control.confirmedText}
              pending={control.textPending}
              send={control.send}
              onClose={() => setKeyboard(false)}
            />
          )}
          {!accessUnavailable && !control.leaving && <InterventionDetails
            targetId={target.target_id}
            taskId={task?.task_id}
          />}
          <Sheet open={more} onClose={() => setMore(false)} title={target.label}>
            <div className="group">
              <ListRow icon={AppWindow} title="Apps and windows" subtitle="Find, focus and open apps" chevron
                onClick={() => { setMore(false); ui.push({ screen: "windows", targetId: target.target_id }); }} />
              {target.kind === "output" && <ListRow icon={Pencil} title="Rename this screen" subtitle="Give it a name you’ll recognize"
                onClick={() => { setMore(false); setRenaming(true); }} />}
            </div>
            {lockControls}
            {actualNote}
          </Sheet>
          {renaming && <RenameScreen target={target} onClose={() => setRenaming(false)} />}
        </>
      )}
      {voiceStart}
    </Screen>
  );
}
