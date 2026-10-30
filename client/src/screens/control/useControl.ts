// SPDX-License-Identifier: Apache-2.0
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type RefObject,
} from "react";
import { useApp, useHost, useLink, describeError } from "../../app/context";
import type { HostSession } from "../../host/session";
import { commandResult } from "../../host/commandResult";
import { useKeepAwake } from "../../platform/awake";
import { announce } from "../../design/Announcer";
import { toast } from "../../design/Toast";
import { useNow } from "../../design/time";
import { refreshFromSnapshot } from "../../state/projection";
import type {
  CommandReceipt,
  InputAction,
  LeaseView,
} from "../../protocol/types";
import { useLeaving } from "./leaving";
import { useLocalText } from "./local";
import { useRoute } from "../../shell/Screen";
import {
  frameFresh,
  FRAME_STALE_MS,
  InputSender,
  renewalDelay,
  scheduleRenewal,
} from "./lease";
import { desktopBlocker, inputBlocker } from "./readiness";
import type { Picture } from "./useMedia";
import { useVoiceHold, voiceHoldsLease } from "./voiceHold";

const textActions = new WeakMap<HostSession, Map<string, InputAction>>();
const senders = new WeakMap<HostSession, InputSender>();
const releasedTargets = new WeakMap<HostSession, Map<string, string>>();
// Views that asked to control each display. A lease this phone holds is left
// only when no view still wants it, so a hidden view never drops another's.
const armedTargets = new WeakMap<HostSession, Map<string, Set<object>>>();
function armed(session: HostSession, id: string): Set<object> {
  let targets = armedTargets.get(session);
  if (!targets) {
    targets = new Map();
    armedTargets.set(session, targets);
  }
  let views = targets.get(id);
  if (!views) {
    views = new Set();
    targets.set(id, views);
  }
  return views;
}
export function releaseTarget(
  session: HostSession,
  id: string,
): string | undefined {
  return releasedTargets.get(session)?.get(id);
}
export function inputSender(session: HostSession): InputSender {
  let sender = senders.get(session);
  if (!sender) {
    const actions = new Map<string, InputAction>();
    textActions.set(session, actions);
    sender = new InputSender((env) => {
      const key = `${env.lease_id}/${env.seq}`;
      if (env.action.action === "text" || env.action.action === "paste") actions.set(key, env.action);
      const sent = session.sendInput(env);
      if (!sent) actions.delete(key);
      return sent;
    });
    senders.set(session, sender);
  }
  return sender;
}
export function releaseHuman(session: HostSession, lease: LeaseView): void {
  const pending = Object.values(session.commands.get()).some((c) =>
    c.body.command === "release_lease" && c.body.lease_id === lease.lease_id
    && ["sending", "accepted", "duplicate"].includes(c.phase)
    && !session.state.get().commandStages[c.id]?.error && Date.now() - c.issuedAt < 8_000);
  if (pending) return;
  const target = session.state
    .get()
    .targets.find((t) => t.target_id === lease.target_id);
  let targets = releasedTargets.get(session);
  if (!targets) {
    targets = new Map();
    releasedTargets.set(session, targets);
  }
  targets.set(lease.lease_id, lease.target_id);
  inputSender(session).retire(lease, target?.geometry.revision ?? 0);
  void session
    .command(
      { command: "release_lease", lease_id: lease.lease_id },
      { label: "Leaving control" },
    )
    .catch((e: unknown) =>
      toast(describeError(e, "The computer"), { tone: "stop" }),
    );
}

/** A lease request settles on the computer after admission. Wait for that
 * outcome; a refusal or an unconfirmed result is shown, never taken as granted. */
export async function grantedLease(
  session: HostSession,
  receipt: CommandReceipt | null,
  computer: string,
): Promise<LeaseView | null> {
  if (!receipt) return null;
  try {
    const effect = await commandResult(session, receipt);
    return effect?.kind === "lease_granted" ? effect.lease : null;
  } catch (e) {
    toast(describeError(e, computer), { tone: "stop" });
    return null;
  }
}

export function pauseSupervised(session: HostSession, targetId: string): void {
  if (session.link.get().state !== "online") return;
  const state = session.state.get(),
    view = state.control[targetId];
  if (view?.input_state !== "agent") return;
  const runId =
    view.lease?.owner.kind === "agent" ? view.lease.owner.run_id : null;
  const taskId = runId ? state.runs[runId]?.task_id : null;
  const inFlight = Object.values(session.commands.get()).some(
    (c) =>
      (c.phase === "sending" ||
        c.phase === "unknown" ||
        c.phase === "accepted") &&
      ((c.body.command === "stop_task" &&
        c.body.task_id === taskId &&
        c.issuedAt >= (runId ? (state.runs[runId]?.admitted_at ?? 0) : 0)) ||
        (c.body.command === "pause_input" &&
          c.body.target_id === targetId &&
          c.issuedAt >= (view.lease?.granted_at ?? 0))),
  );
  if (inFlight) return;
  announce("Pause requested");
  void session
    .command(
      { command: "pause_input", target_id: targetId },
      { label: "Pause requested" },
    )
    .catch((e: unknown) =>
      toast(describeError(e, "The computer"), { tone: "stop" }),
    );
}

/** This route is the one on screen, and the app is in the foreground. */
export function useOnScreen(): boolean {
  const { active } = useRoute();
  const [foreground, setForeground] = useState(
    () => document.visibilityState !== "hidden",
  );
  useEffect(() => {
    const changed = () => setForeground(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", changed);
    return () => document.removeEventListener("visibilitychange", changed);
  }, []);
  return active && foreground;
}

export function useVisible(): boolean {
  const visible = useOnScreen();
  useEffect(() => {
    if (!visible) return;
    const frame = requestAnimationFrame(() => {
      const heading = document.querySelector<HTMLElement>(
        ".tab:not(.is-hidden) .route:not(.is-hidden) :is(.bar__title, .screen__title, .ctx__title)",
      );
      // Focus already on this screen was put there on purpose: the shell
      // restoring the last control, or a finished send pointing at its result.
      const held = document.activeElement;
      if (held && held !== document.body && heading?.closest(".route")?.contains(held)) return;
      if (heading) {
        heading.tabIndex = -1;
        heading.focus({ preventScroll: true });
      }
    });
    return () => cancelAnimationFrame(frame);
  }, [visible]);
  return visible;
}

export function useControl(
  targetId: string | undefined,
  picture: RefObject<Picture>,
  visible: boolean,
) {
  const { session, profile, platform } = useApp();
  const online = useLink().state === "online";
  const now = useNow();
  const view = useHost((s) => (targetId ? s.control[targetId] : undefined));
  const target = useHost((s) =>
    s.targets.find((t) => t.target_id === targetId),
  );
  const caps = useHost((s) => s.capabilities);
  const grants = useHost((s) => s.grants);
  const inputReason = inputBlocker(caps, target, view, grants);
  const boot = useHost((s) => s.bootEpoch);
  const [intent, setIntent] = useState(false);
  const intentRef = useRef(false);
  const self = useRef({}).current;
  const [reconciled, setReconciled] = useState(false);
  const [savedText, setSavedText] = useLocalText(`control.pending.${profile.profileId}.${targetId ?? "none"}`);
  const [pasteOffered, setPasteOffered] = useState(false);
  const [confirmedText, setConfirmedText] = useState<{ text: string; at: number } | null>(null);
  const [textPending, setTextPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const lease = view?.lease;
  const leaving = useLeaving(session, lease);
  const hold = useVoiceHold(session, profile.profileId, targetId);
  const ours =
    lease?.owner.kind === "human" &&
    lease.owner.device_id === profile.deviceId &&
    lease.state === "active" &&
    lease.boot_epoch === boot;
  // Voice control's lease is this device's too, but not for touch input, and
  // only voice control ends it.
  const voice = ours && !!hold && (hold.leaseId === null || hold.leaseId === lease.lease_id);
  const mine = ours && !voice;
  useKeepAwake(platform, visible && online && mine);
  const isFresh =
    picture.current.connected &&
    frameFresh(picture.current.at, now) &&
    picture.current.revision === target?.geometry.revision;
  const enabled =
    visible &&
    online &&
    reconciled &&
    isFresh &&
    mine &&
    intent &&
    !leaving &&
    view?.input_state === "human" &&
    !inputReason && !!grants?.control_input && !!grants.view;
  const lifecycle = useRef(new Map<string, number>());

  const leave = useCallback(() => {
    intentRef.current = false;
    setIntent(false);
    if (targetId) armed(session, targetId).delete(self);
    const current = targetId
      ? session.state.get().control[targetId]?.lease
      : null;
    if (
      current?.owner.kind === "human" &&
      current.owner.device_id === profile.deviceId &&
      current.state === "active" &&
      !voiceHoldsLease(session, current)
    ) {
      releaseHuman(session, current);
    }
  }, [session, targetId, profile.deviceId, self]);

  useEffect(() => {
    let disposed = false;
    setReconciled(false);
    if (!visible || !online || !targetId) return;
    void session.query({ kind: "snapshot" }).then(
      (snapshot) => {
        if (disposed) return;
        session.state.set((state) =>
          refreshFromSnapshot(state, snapshot, Date.now()),
        );
        setReconciled(true);
        setError(null);
      },
      (e: unknown) => {
        if (!disposed) setError(describeError(e, profile.name));
      },
    );
    return () => {
      disposed = true;
    };
  }, [session, profile.name, targetId, visible, online]);

  useEffect(() => {
    if (!targetId || !visible) return;
    const generation = (lifecycle.current.get(targetId) ?? 0) + 1;
    lifecycle.current.set(targetId, generation);
    const close = () => {
      leave();
      pauseSupervised(session, targetId);
    };
    const hidden = () => {
      if (document.visibilityState === "hidden") close();
    };
    window.addEventListener("pagehide", close);
    document.addEventListener("visibilitychange", hidden);
    return () => {
      window.removeEventListener("pagehide", close);
      document.removeEventListener("visibilitychange", hidden);
      queueMicrotask(() => {
        if (lifecycle.current.get(targetId) === generation) close();
      });
    };
  }, [session, targetId, visible, leave]);

  // A lease granted after navigation/receipt delay must be released too.
  useEffect(() => {
    if (!mine || (visible && intentRef.current)) return;
    const wanted = targetId ? armed(session, targetId) : null;
    if (wanted && [...wanted].some((v) => v !== self)) return;
    leave();
  }, [mine, lease?.lease_id, visible, leave, session, targetId, self]);

  useEffect(() => {
    const supervised =
      lease?.owner.kind === "agent" &&
      lease.state === "active" &&
      lease.boot_epoch === boot &&
      view?.input_state === "agent";
    if (
      !lease ||
      (!supervised && (!mine || !intent)) ||
      !visible ||
      !online ||
      !reconciled || !!inputReason || !grants?.control_input || !grants.view
    )
      return;
    const delay = renewalDelay(lease.renewal_window_ms, visible, isFresh);
    if (delay === null) {
      if (mine)
        inputSender(session).release(lease, target?.geometry.revision ?? 0);
      return;
    }
    return scheduleRenewal(
      lease.renewal_window_ms,
      () => {
        const current = targetId
          ? session.state.get().control[targetId]
          : undefined;
        const stopping = Object.values(session.commands.get()).some(
          (c) =>
            (c.phase === "sending" || c.phase === "unknown") &&
            ((c.body.command === "pause_input" &&
              c.body.target_id === targetId) ||
              (c.body.command === "stop_task" &&
                session.state.get().tasks[c.body.task_id]?.target_id ===
                  targetId) ||
              (c.body.command === "release_lease" &&
                c.body.lease_id === lease.lease_id)),
        );
        return (
          !stopping &&
          !inputSender(session).retired(lease) &&
          !desktopBlocker(current, session.state.get().capabilities, session.state.get().grants) &&
          current?.lease?.state === "active" &&
          current.lease.lease_id === lease.lease_id &&
          document.visibilityState !== "hidden" &&
          picture.current.connected &&
          frameFresh(picture.current.at, Date.now()) &&
          picture.current.revision === target?.geometry.revision &&
          (supervised || intentRef.current)
        );
      },
      () => {
        session.renewLease(lease.lease_id, lease.generation);
      },
    );
  }, [
    session,
    lease?.lease_id,
    lease?.generation,
    lease?.renewal_window_ms,
    mine,
    boot,
    view?.input_state,
    visible,
    intent,
    online,
    reconciled,
    inputReason,
    grants,
    isFresh,
    targetId,
    target?.geometry.revision,
    picture,
  ]);

  useEffect(() => {
    if (!targetId || !visible) return;
    const started = Date.now();
    const timer = setInterval(() => {
      if (Date.now() - started < FRAME_STALE_MS && picture.current.at === null)
        return;
      if (
        !picture.current.connected ||
        !frameFresh(picture.current.at, Date.now()) ||
        picture.current.revision !== target?.geometry.revision
      )
        pauseSupervised(session, targetId);
    }, 500);
    return () => clearInterval(timer);
  }, [session, targetId, target?.geometry.revision, visible, picture]);

  useEffect(() => {
    if (!online || !mine) setTextPending(false);
    if (view?.desktop_lock === "locked") { setPasteOffered(false); leave(); }
  }, [online, mine, view?.desktop_lock, leave]);

  useEffect(
    () =>
      session.onInputAck((ack) => {
        if (ack.lease_id !== lease?.lease_id) return;
        const key = `${ack.lease_id}/${ack.seq}`;
        const action = textActions.get(session)?.get(key);
        if (action?.action === "text" || action?.action === "paste") {
          setTextPending(false);
          if (ack.outcome === "injected") {
            setConfirmedText({ text: action.text, at: Date.now() });
            setError(null);
            setSavedText("");
            setPasteOffered(false);
          } else {
            setSavedText(action.text);
            setPasteOffered(ack.refusal?.kind === "text_unavailable" && ack.refusal.paste_available);
          }
          queueMicrotask(() => textActions.get(session)?.delete(key));
        }
        if (ack.outcome === "injected") return;
        const messages = {
          stale_lease:
            "Control ended on the computer. Enable control again after reviewing its state.",
          stale_geometry:
            "The display changed. Review the new picture before using input.",
          out_of_order: "The computer discarded an older input action.",
          // It may or may not have happened, so nothing is sent again,
          // including this action; the person looks before acting.
          unknown:
            "The computer couldn’t confirm whether that input happened. Check the picture before continuing.",
          failed: "The computer couldn’t apply that input.",
        };
        const message = ack.refusal?.kind === "locked"
          ? "Computer is locked. Unlock on the computer before enabling control."
          : ack.refusal?.kind === "text_unavailable"
            ? `Nothing was typed. The computer’s keyboard cannot type: ${ack.refusal.characters}. ${ack.refusal.paste_available ? "Paste is available with your permission." : "Paste is not available in this desktop session."}`
            : messages[ack.outcome];
        setError(message);
        announce(message);
        if (
          ack.refusal?.kind === "locked" || ack.outcome === "stale_lease" ||
          ack.outcome === "unknown"
        )
          leave();
      }),
    [session, lease?.lease_id, leave],
  );

  return {
    view,
    target,
    mine,
    voice,
    enabled,
    isFresh,
    reconciled,
    leaving,
    error,
    savedText,
    pasteOffered,
    textPending,
    confirmedText,
    editText: () => { setPasteOffered(false); setError(null); setConfirmedText(null); },
    discardText: () => { setSavedText(""); setPasteOffered(false); },
    leave,
    arm: () => {
      intentRef.current = true;
      setIntent(true);
      if (targetId) armed(session, targetId).add(self);
      setError(null);
    },
    settled: (lease: LeaseView | null) => {
      if (lease && !intentRef.current && !voiceHoldsLease(session, lease)) releaseHuman(session, lease);
    },
    send: (action: InputAction): boolean => {
      const state = session.state.get(),
        current = targetId ? state.control[targetId] : undefined,
        liveLease = current?.lease;
      if (
        !enabled ||
        !liveLease ||
        liveLease.lease_id !== lease?.lease_id ||
        liveLease.state !== "active" ||
        current?.input_state !== "human" ||
        inputBlocker(state.capabilities, state.targets.find((t) => t.target_id === targetId), current, state.grants) ||
        !state.grants?.control_input || !state.grants.view ||
        picture.current.revision !==
          state.targets.find((t) => t.target_id === targetId)?.geometry
            .revision ||
        !picture.current.connected ||
        !intentRef.current ||
        document.visibilityState === "hidden" ||
        !frameFresh(picture.current.at, Date.now())
      )
        return false;
      setConfirmedText(null);
      if (action.action === "text" || action.action === "paste") {
        if (textPending) return false;
        setSavedText(action.text);
        setTextPending(true);
      }
      const sent = inputSender(session).send(liveLease, target?.geometry.revision ?? 0, action);
      if (!sent) setTextPending(false);
      return sent;
    },
  };
}
