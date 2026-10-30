// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from "react";
import { Check } from "lucide-react";
import { shallowEqual, useApp, useHost, useLink, describeError } from "../../app/context";
import { Button } from "../../design/Button";
import { TextArea } from "../../design/Field";
import { Empty, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Age } from "../../design/Age";
import { CommandError, OutcomeUnknownError } from "../../host/session";
import { commandResult } from "../../host/commandResult";
import { uuidv7 } from "../../protocol/ids";
import type { Availability, Command, CommandReceipt, ControlTarget } from "../../protocol/types";
import { Screen } from "../../shell/Screen";
import { NotAllowed } from "../../shell/NotAllowed";
import type { RouteProps } from "../types";
import { agentUnavailable, goalKey, preferredComputerAgent, targetBlocker, usesComputerItself, COMPUTER_TOOLS } from "./computerReview";
import { ConnectionNote } from "./Feedback";
import { Unlock, useLocked } from "./Unlock";
import { writerActivity, writerHold, writerHoldText } from "../talk/intent";
import "./control.css";

type Start = Extract<Command, { command: "create_task" }>;
interface Draft { goal: string; target: string; harness: string; model: string; attempt?: { id: string; body: Start } }
const EMPTY: Draft = { goal: "", target: "", harness: "", model: "" };
function read(key: string, projectId: string): Draft {
  try {
    const value = JSON.parse(localStorage.getItem(key) ?? "null") as Draft | null;
    if (value && [value.goal, value.target, value.harness, value.model].every((s) => typeof s === "string")) {
      const attempt = value.attempt;
      if (attempt && (typeof attempt.id !== "string" || attempt.body?.command !== "create_task" || attempt.body.kind !== "computer_use"
        || attempt.body.project_id !== projectId || attempt.body.target_id !== value.target || attempt.body.harness !== value.harness || attempt.body.prompt !== value.goal.trim()))
        return { ...value, attempt: undefined };
      return value;
    }
  } catch { /* Keep editing available if local storage is unreadable. */ }
  return EMPTY;
}
export default function ComputerReview({ route }: RouteProps<"computer_use_review">) {
  const { session, profile, ui } = useApp();
  const key = goalKey(profile.profileId, route.projectId);
  const [draft, setDraft] = useState(() => read(key, route.projectId));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const errorRef = useRef<HTMLDivElement>(null);
  useEffect(() => { if (error) { errorRef.current?.focus({ preventScroll: true }); errorRef.current?.scrollIntoView?.({ block: "nearest" }); } }, [error]);
  const [checked, setChecked] = useState(false);
  const online = useLink().state === "online";
  const project = useHost((s) => s.projects[route.projectId]);
  const held = useHost((s) => writerHold(s.projects[route.projectId]?.active_writer, s), shallowEqual);
  // Agents use the agent browser through its own tools, never as a screen.
  const targets = useHost((s) => s.targets).filter((t) => t.kind !== "browser");
  const agents = useHost((s) => s.harnesses);
  const grants = useHost((s) => s.grants);
  const caps = useHost((s) => s.capabilities);
  const missingGrants = ([['tasks', 'tasks'], ['computer_use', 'computer-use'], ['view', 'view'], ['control_input', 'control']] as const)
    .filter(([grant]) => grants?.[grant] === false).map(([, permission]) => permission);
  const controls = useHost((s) => s.control);
  const at = useHost((s) => s.lastHeardAt);
  const selectedTarget = targets.find((t) => t.target_id === draft.target);
  // Until the person picks an agent, the one new work goes to is chosen while it's ready, with its default model.
  const preferred = draft.harness ? undefined : preferredComputerAgent(agents, project);
  const harness = draft.harness || preferred?.kind || "";
  const modelId = draft.harness ? draft.model : preferred?.models.find((m) => m.is_default)?.id ?? "";
  const selectedAgent = agents.find((h) => h.kind === harness);
  const model = selectedAgent?.models.find((m) => m.id === modelId);
  const itself = !!selectedAgent && usesComputerItself(selectedAgent.kind);
  const save = (patch: Partial<Draft>) => {
    const next = { ...draft, ...patch };
    setDraft(next);
    try { localStorage.setItem(key, JSON.stringify(next)); return true; }
    catch { setError("This draft couldn’t be saved on this device. Keep this app open until storage is available."); return false; }
  };
  useEffect(() => {
    if (!online) return;
    let disposed = false;
    void Promise.all([session.refreshHarnesses(), session.query({ kind: "control_targets" }).then((r) => {
      if (!disposed) session.state.set((s) => ({ ...s, targets: r.targets }));
    })]).catch((e: unknown) => { if (!disposed) setError(describeError(e, profile.name)); });
    return () => { disposed = true; };
  }, [session, online, profile.name]);
  const locked = useLocked();
  const targetReasonFor = (target: ControlTarget): string | null => targetBlocker(caps, target, controls[target.target_id], grants);
  const targetReason = selectedTarget ? targetReasonFor(selectedTarget) : "Choose a target reported by the computer.";
  const occupied = selectedTarget && (controls[selectedTarget.target_id]?.lease?.state === "active" || controls[selectedTarget.target_id]?.private_step !== null && controls[selectedTarget.target_id]?.private_step !== undefined);
  const reason = !project ? "Choose a project on this computer first." : !grants?.computer_use || !grants.tasks || !grants.view || !grants.control_input ? "This device doesn’t have permission to start computer use."
    : caps?.computer_use.state !== "available" ? `Computer use is unavailable. ${availabilityDetail(caps?.computer_use) ?? "Check the computer’s desktop permissions and local Stop indicator."}`
      : !selectedAgent ? "Choose an eligible agent." : agentUnavailable(selectedAgent) ?? targetReason ?? (occupied ? "This target is already in use or in a private step." : null)
        ?? (modelId && !model ? "That model is no longer in the computer’s catalog." : null)
        ?? (held ? (held.unresolved ? `${writerHoldText(held, project.name)} Check it or choose another project first.` : `${writerActivity(held, project.name)}. Stop it or choose another project first.`) : null);
  const accepted = async (receipt: CommandReceipt) => {
    const result = await commandResult(session, receipt);
    if (result?.kind !== "task_created") throw new Error("It may have started, but the computer hasn’t said which task it is. Check Tasks before starting another.");
    localStorage.removeItem(key);
    ui.finish(["computer_use_review"]);
    ui.push({ screen: "computer_use", taskId: result.task_id });
  };
  const submit = async () => {
    if (!project || !selectedAgent || !selectedTarget || reason || !draft.goal.trim() || !online || busy || (draft.attempt && !checked)) return;
    const attempt = draft.attempt ?? { id: uuidv7(Date.now()), body: {
      command: "create_task", project_id: project.project_id, kind: "computer_use", mode: "work", title: draft.goal.trim().split("\n")[0]?.slice(0, 120) ?? null,
      prompt: draft.goal.trim(), harness: selectedAgent.kind, model: model?.id ?? null, effort: null,
      target_id: selectedTarget.target_id, bundle: null, mission: null, placement: { mode: "start_now" },
    } satisfies Start };
    if (!save({ attempt, harness: selectedAgent.kind, model: model?.id ?? "" })) return;
    setBusy(true); setError(null); setChecked(false);
    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      const unanswered = new Promise<never>((_, reject) => { timeout = setTimeout(() => reject(new OutcomeUnknownError(attempt.id)), 8_000); });
      await accepted(await Promise.race([unanswered, session.command(attempt.body, { commandId: attempt.id, label: "Start computer use" })]));
    }
    catch (e) {
      if (e instanceof CommandError && e.error.code !== "outcome_unknown") save({ attempt: undefined });
      setError(e instanceof OutcomeUnknownError ? "The computer hasn’t confirmed Start. It may already be running. Check this request before doing anything again." : describeError(e, profile.name));
    } finally { if (timeout) clearTimeout(timeout); setBusy(false); }
  };
  const check = async () => {
    if (!draft.attempt) return;
    setBusy(true);
    try {
      const r = await session.query({ kind: "command_status", command_id: draft.attempt.id });
      if (r.receipt && r.receipt.status !== "rejected") await accepted(r.receipt);
      else if (r.receipt?.error) { save({ attempt: undefined }); setError(r.receipt.error.message); }
      else { setChecked(true); setError("The computer has no record of this request. Retry sends the same reviewed request and identity."); }
    } catch (e) { setError(describeError(e, profile.name)); }
    finally { setBusy(false); }
  };
  return <Screen title="Start computer use" subtitle={project ? `${project.name} · ${profile.name}` : profile.name} className="control-screen computer-review"
    footer={<div className="computer-review__footer"><Button variant="primary" block pending={busy} disabled={!online || (!!draft.attempt && !checked ? false : !!reason || !draft.goal.trim())}
      onClick={() => void (draft.attempt && !checked ? check() : submit())}>{draft.attempt ? checked ? "Retry reviewed Start" : "Check Start request" : "Start computer use"}</Button></div>}>
    <ConnectionNote />
    {missingGrants.map(permission => <NotAllowed key={permission} what="start computer use" permission={permission} />)}
    {error && <div ref={errorRef} tabIndex={-1} role="alert"><Notice tone="unknown">{error}</Notice></div>}
    {draft.attempt && <Notice tone="unknown">Start is awaiting confirmation. Your reviewed goal and choices are kept on this device.</Notice>}
    {at !== null && <p className="fine">Computer’s last report · <Age at={at} suffix=" ago" /></p>}
    <Section title="Target">
      <p className="fine">The agent works on the actual desktop. Local mouse and keyboard input still work and can interfere.</p>
      {!targets.length && <Empty title="No targets reported">Enable desktop sharing on the computer, then return here.</Empty>}
      <div className="computer-choices" role="group" aria-label="Computer targets">{targets.map((t) => {
        const unavailable = targetReasonFor(t);
        return <button key={t.target_id} type="button" className={`computer-choice${unavailable ? " computer-choice--unavailable" : ""}`} aria-pressed={draft.target === t.target_id} disabled={!!unavailable || !!draft.attempt}
          onClick={() => save({ target: t.target_id })}><strong>{t.label}</strong><span>{t.kind === "window" ? "Window" : t.kind === "desktop" ? "Desktop" : "Display"} · {t.geometry.width} × {t.geometry.height}</span>{unavailable && <Why>{unavailable}</Why>}{draft.target === t.target_id && <Selected />}</button>;
      })}</div>
      {locked && <Unlock />}
    </Section>
    <Section title="Agent"><div className="computer-choices" role="group" aria-label="Eligible agents">{agents.map((agent) => {
      const unavailable = agentUnavailable(agent);
      return <button key={agent.kind} type="button" className={`computer-choice${unavailable ? " computer-choice--unavailable" : ""}`} aria-pressed={harness === agent.kind} disabled={!!unavailable || !!draft.attempt}
        onClick={() => save({ harness: agent.kind, model: agent.models.find((m) => m.is_default)?.id ?? "" })}><strong>{agent.display_name}</strong>{unavailable ? <Why>{unavailable}</Why> : <span>Ready for computer use</span>}{harness === agent.kind && <Selected />}</button>;
    })}</div>{!agents.length && <Empty title="No agents reported">Connect to the computer to check its agents.</Empty>}</Section>
    {selectedAgent && <Section title="Model and account">
      <div className="computer-choices" role="group" aria-label="Models">{selectedAgent.models.length ? selectedAgent.models.map((m) => <button key={m.id} type="button" className="computer-choice" aria-pressed={modelId === m.id} disabled={!!draft.attempt}
        onClick={() => save({ harness, model: m.id })}><strong>{m.label}</strong>{m.suggested_for_computer_use && <span>Suggested for computer use</span>}{modelId === m.id && <Selected />}</button>) : <p>Native default model. The computer hasn’t reported its name yet.</p>}</div>
      <p className="fine">Account: {selectedAgent.account.state === "signed_in" ? selectedAgent.account.account_label ?? "Signed in on the computer; account name not reported" : "Not ready"}</p>
      <p>Pictures of {selectedTarget?.label ?? "the chosen target"} go to {selectedAgent.display_name}{itself ? "" : "’s computer specialist"}{model ? ` using ${model.label}` : ""}. Earlier pictures stay in the conversation.</p>
    </Section>}
    <Section title="What the agent can do"><ul className="computer-tools">{COMPUTER_TOOLS.map((tool) => <li key={tool}>{tool}</li>)}</ul><p className="fine">{itself ? `${selectedAgent.display_name} works on this target with these tools only. It can’t run commands or change files.` : "The agent’s computer specialist works on this target. It cannot run shell commands or write files through these controls."}</p></Section>
    <TextArea label="Goal" value={draft.goal} onChange={(goal) => save({ goal })} rows={3} disabled={!!draft.attempt} hint="Saved on this device until it starts." />
    {reason && <Notice tone="warn">{reason}</Notice>}
  </Screen>;
}

function Why({ children }: { children: string }) { return <span className="computer-choice__why">Can’t choose · {children}</span>; }

function Selected() { return <span className="computer-choice__selected"><Check size={16} aria-hidden />Selected</span>; }

/** The computer's own explanation, when it gave one. */
function availabilityDetail(a: Availability | undefined): string | null {
  if (!a) return null;
  return "reason" in a ? a.reason : "remedy" in a ? a.remedy : null;
}
