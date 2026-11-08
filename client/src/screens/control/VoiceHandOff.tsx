// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { Bot } from "lucide-react";
import { describeError, useApp, useHost, useLink, useUi } from "../../app/context";
import { Button } from "../../design/Button";
import { Notice, availabilityText } from "../../design/Notice";
import { StateLine } from "../../design/StateLine";
import { commandResult } from "../../host/commandResult";
import { CommandError, OutcomeUnknownError } from "../../host/session";
import { uuidv7 } from "../../protocol/ids";
import type { Command, Grants, HostCapabilities, VoiceSessionView } from "../../protocol/types";
import { Store, useStore } from "../../state/store";
import { agentUnavailable, goalKey, preferredComputerAgent, restorePreviousGoal, suggestGoal } from "./computerReview";
import { dropVoice } from "./voiceHold";

type Start = Extract<Command, { command: "create_task" }>;

/** A hand-off under way, kept here rather than in the voice session's
 * view: stopping voice control ends that view, and the hand-off must still
 * show how it went until computer use has started or plainly hasn't. */
export interface HandOff {
  instruction: string;
  projectId: string;
  phase: "stopping" | "starting" | "unknown" | "checking" | "unrecorded" | "failed";
  /** The start request as sent, kept here whether or not this phone could
   * also save it, so an unanswered start is checked under its own identity. */
  attempt?: { id: string; body: Start };
  /** Start computer use's draft for the project before this request took
   * its place there, put back once the start has settled. */
  before?: string | null;
  /** Voice control has let go of the display. */
  stopped: boolean;
  error?: string;
}
export const handOffs = new Store<Record<string, HandOff>>({});
const set = (sessionId: string, next: HandOff | null) =>
  handOffs.set((all) => {
    const rest = { ...all };
    if (next) rest[sessionId] = next; else delete rest[sessionId];
    return rest;
  });

/** Why this device can't hand a command to a computer-use agent now, or null. */
export function handOffBlocker(online: boolean, projectId: string | null, caps: HostCapabilities | null, grants: Grants | null): string | null {
  if (!online) return "Reconnect to the computer to hand this to an agent.";
  if (!grants?.tasks || !grants.computer_use || !grants.view || !grants.control_input) return "This device doesn’t have permission to start computer use.";
  if (caps?.computer_use.state !== "available") {
    const why = caps ? availabilityText(caps.computer_use).detail : null;
    return `Computer use is unavailable on the computer.${why ? ` ${why}` : ""}`;
  }
  if (!projectId) return "Choose a project in Control first: the agent works in it.";
  return null;
}

/** Start computer use's draft for a project: read as text, so it can be put
 * back as it was. */
function readDraft(key: string): string | null {
  return localStorage.getItem(key);
}
function writeDraft(key: string, text: string | null): boolean {
  try { if (text === null) localStorage.removeItem(key); else localStorage.setItem(key, text); return true; } catch { return false; }
}

/** The computer offers what voice control can't do to an agent. The exact
 * words, the agent and the display are shown; one tap stops voice control,
 * which lets go of the display, and starts computer use on it the way Start
 * computer use does: the request is recorded there first, under the same
 * identity, so an unanswered start is checked rather than sent twice. */
export function VoiceHandOff({ value }: { value: VoiceSessionView }) {
  const { session, profile, ui } = useApp();
  const online = useLink().state === "online";
  const grants = useHost((s) => s.grants);
  const caps = useHost((s) => s.capabilities);
  const agents = useHost((s) => s.harnesses);
  const chosen = useUi((s) => s.projectId);
  const handOff = useStore(handOffs, (all) => all[value.session_id]);
  const projectId = handOff?.projectId ?? chosen;
  const project = useHost((s) => (projectId ? s.projects[projectId] : undefined));
  const display = useHost((s) => s.targets.find((t) => t.target_id === value.target_id));
  const offer = value.offer && !value.ended ? value.offer : undefined;
  const [dismissRefused, setDismissRefused] = useState(false);
  useEffect(() => {
    if (offer && online) void session.refreshHarnesses().catch(() => undefined);
  }, [session, online, !!offer]);
  if (!handOff && !offer) return null;
  // The words shown are the words sent.
  const instruction = handOff?.instruction ?? offer?.instruction.trim() ?? "";
  const agent = preferredComputerAgent(agents, project);
  const model = agent?.models.find((m) => m.is_default);
  const blocker = handOffBlocker(online, projectId, caps, grants)
    ?? (!agent ? (agents.length ? "No agent on the computer can use it right now." : "Checking the computer’s agents…") : agentUnavailable(agent));
  const busy = handOff?.phase === "stopping" || handOff?.phase === "starting" || handOff?.phase === "checking";
  const key = (projectId: string) => goalKey(profile.profileId, projectId);
  /** Sends `attempt` (again, under the same identity) and settles it. */
  const send = async (state: HandOff, attempt: { id: string; body: Start }) => {
    const sessionId = value.session_id;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      const unanswered = new Promise<never>((_, reject) => { timeout = setTimeout(() => reject(new OutcomeUnknownError(attempt.id)), 8_000); });
      const receipt = await Promise.race([unanswered, session.command(attempt.body, { commandId: attempt.id, label: "Start computer use" })]);
      const result = await Promise.race([unanswered, commandResult(session, receipt)]);
      if (result?.kind !== "task_created") throw new OutcomeUnknownError(attempt.id);
      started(state, result.task_id);
    } catch (e) {
      const unknown = e instanceof OutcomeUnknownError || (e instanceof CommandError && e.error.code === "outcome_unknown");
      if (unknown) set(sessionId, { ...state, attempt, phase: "unknown" });
      else failed(state, e);
    } finally { if (timeout) clearTimeout(timeout); }
  };
  const started = (state: HandOff, taskId: string) => {
    if (state.attempt) restorePreviousGoal(key(state.projectId), state.attempt.id, state.before);
    set(value.session_id, null);
    ui.push({ screen: "computer_use", taskId });
  };
  const failed = (state: HandOff, e: unknown) => {
    if (state.stopped && state.attempt) restorePreviousGoal(key(state.projectId), state.attempt.id, state.before);
    set(value.session_id, {
      ...state,
      attempt: undefined,
      phase: "failed",
      error: state.stopped ? `Voice control stopped, but the agent didn’t start. ${describeError(e, profile.name)}` : describeError(e, profile.name),
    });
  };
  /** What the computer knows of the unanswered request, as Start computer
   * use checks it: started, refused, or never received. */
  const check = async (state: HandOff) => {
    const attempt = state.attempt;
    if (!attempt) return;
    set(value.session_id, { ...state, phase: "checking" });
    // Only the start's own result can be a definite failure. A refusal of
    // the status question itself, or a failure to read it, leaves the start
    // unknown, and its request kept.
    let r;
    try {
      r = await session.query({ kind: "command_status", command_id: attempt.id });
    } catch (e) {
      set(value.session_id, { ...state, phase: "unknown", error: describeError(e, profile.name) });
      return;
    }
    if (!r.receipt) { set(value.session_id, { ...state, phase: "unrecorded" }); return; }
    if (r.receipt.status === "rejected") {
      if (r.receipt.error) failed(state, new Error(r.receipt.error.message));
      else set(value.session_id, { ...state, phase: "unrecorded" });
      return;
    }
    try {
      const result = await commandResult(session, r.receipt);
      if (result?.kind === "task_created") started(state, result.task_id);
      else set(value.session_id, { ...state, phase: "unknown" });
    } catch (e) {
      // The computer has the request and says it failed: that is known.
      if (e instanceof CommandError && e.error.code !== "outcome_unknown") failed(state, e);
      else set(value.session_id, { ...state, phase: "unknown", error: describeError(e, profile.name) });
    }
  };
  const start = async () => {
    // Again only while voice control still runs: once it has stopped, the
    // way on is Start computer use.
    if (!agent || !project || blocker || (handOff && (handOff.stopped || handOff.phase !== "failed")) || !instruction) return;
    const sessionId = value.session_id;
    let state: HandOff = { instruction, projectId: project.project_id, phase: "stopping", stopped: false };
    set(sessionId, state);
    try {
      // Voice control lets go of the display first: an agent can't be given
      // a display someone else holds.
      const stop = await session.command({ command: "voice", operation: { operation: "stop", session_id: sessionId } }, { label: "Stop voice control" });
      await commandResult(session, stop);
    } catch (e) {
      failed(state, e);
      return;
    }
    dropVoice(session, profile.profileId, value.target_id, sessionId);
    const attempt = { id: uuidv7(Date.now()), body: {
      command: "create_task", project_id: project.project_id, kind: "computer_use", mode: "work",
      title: instruction.split("\n")[0]?.slice(0, 120) ?? null, prompt: instruction,
      harness: agent.kind, model: model?.id ?? null, effort: null, target_id: value.target_id,
      bundle: null, mission: null, placement: { mode: "start_now" },
    } satisfies Start };
    // Kept here, and also where Start computer use keeps its requests when
    // this phone can save it, before it is sent.
    let before: string | null = null;
    let readable = true;
    try { before = readDraft(key(project.project_id)); } catch { readable = false; }
    state = { ...state, phase: "starting", stopped: true, attempt, before };
    set(sessionId, state);
    // If the earlier goal cannot be read, do not overwrite it.
    if (readable) writeDraft(key(project.project_id), JSON.stringify({ goal: instruction, target: value.target_id, harness: agent.kind, model: model?.id ?? "", attempt, before }));
    await send(state, attempt);
  };
  const close = () => set(value.session_id, null);
  /** Dismiss only when Start computer use already holds this request.
   * Never overwrite a newer draft or close if storage lost the request. */
  const dismissUnknown = (state: HandOff) => {
    let id: string | undefined;
    try {
      id = (JSON.parse(readDraft(key(state.projectId)) ?? "null") as { attempt?: { id?: string } } | null)?.attempt?.id;
    } catch { /* Not our saved request, or storage is unreadable. */ }
    if (state.attempt && id === state.attempt.id) close();
    else setDismissRefused(true);
  };
  return <section className="voice-handoff voice-stack" aria-label="Hand to an agent">
    <p className="voice-label"><Bot aria-hidden size={16} /> {handOff ? "Handing to an agent" : "An agent could do this"}</p>
    <blockquote className="voice-transcript" aria-label="Instruction for the agent">{instruction}</blockquote>
    <dl className="voice-handoff__facts">
      <dt>Agent</dt><dd>{agent ? `${agent.display_name}${model ? ` · ${model.label}` : ""}` : "None ready"}</dd>
      <dt>Display</dt><dd>{display?.label ?? "This display"}</dd>
      {project && <><dt>Project</dt><dd>{project.name}</dd></>}
    </dl>
    {(!handOff || (handOff.phase === "failed" && !handOff.stopped)) && <>
      {handOff?.error && <Notice tone="unknown">{handOff.error}</Notice>}
      <p className="fine">Voice control stops and the agent works on this display while you watch. Stop stays on screen. It uses {agent?.display_name ?? "the agent"}’s own subscription.</p>
      {blocker && <Notice tone="unknown">{blocker}</Notice>}
      <Button variant="primary" block icon={Bot} disabled={!!blocker} onClick={() => void start()}>Hand to {agent?.display_name ?? "an agent"}</Button>
    </>}
    {busy && <StateLine phase="sending" />}
    {handOff?.phase === "unknown" && <>
      <Notice tone="unknown">The computer hasn’t confirmed the agent started. It may already be running. Check this request before starting anything again.{handOff.error ? ` ${handOff.error}` : ""}</Notice>
      <Button variant="primary" block disabled={!online} onClick={() => void check(handOff)}>Check the start</Button>
      <Button variant="quiet" onClick={() => dismissUnknown(handOff)}>Dismiss</Button>
      {dismissRefused && <p className="fine">This request isn’t saved in Start computer use anymore. The hand-off stays here so its identity isn’t lost; check the start before closing it.</p>}
    </>}
    {handOff?.phase === "unrecorded" && handOff.attempt && <>
      <Notice tone="unknown">The computer has no record of this request. Sending it again sends the same request, under the same identity.</Notice>
      <Button variant="primary" block disabled={!online} onClick={() => { const state = { ...handOff, phase: "starting" as const }; set(value.session_id, state); void send(state, handOff.attempt!); }}>Send the same request again</Button>
    </>}
    {handOff?.phase === "failed" && handOff.stopped && <>
      <Notice tone="unknown">{handOff.error}</Notice>
      <Button block onClick={() => { suggestGoal(profile.profileId, handOff.projectId, handOff.instruction); close(); ui.push({ screen: "computer_use_review", projectId: handOff.projectId }); }}>Start computer use</Button>
      <Button variant="quiet" onClick={close}>Dismiss</Button>
    </>}
  </section>;
}
