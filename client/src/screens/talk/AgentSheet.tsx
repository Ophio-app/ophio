// SPDX-License-Identifier: Apache-2.0
//
// Which agent a new task uses. Only agents this build can manage are
// selectable; the others stay listed with the reason and the way forward, so
// nothing is hidden and nothing is offered that would not work.

import { defaultLaunchPolicy, launchChoices, launchRisk, launchSummary, LAUNCH_POLICY_FEATURE, LAUNCH_POLICY_UPDATE } from "../../app/launchPolicy";
import { LaunchLine } from "./LaunchLine";
import { conversationMemory, keepEffort, readEffortMemory, saveEffortMemory, rememberedModel, switchEffort } from "./effortMemory";
import { Bot } from "lucide-react";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useApp, useCommand, useHost, useLink, useUi } from "../../app/context";
import type { AgentChoice } from "../../app/ui";
import { Button } from "../../design/Button";
import { Chip } from "../../design/Chip";
import { ListRow, Section } from "../../design/ListRow";
import { availabilityText } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import type { HarnessInfo, Project, Task } from "../../protocol/types";
import { agentChoiceBlocker, canStart, modeSupport } from "./intent";

import { Age } from "../../design/Age";
import { Notice } from "../../design/Notice";
import { modelChoiceValid, modelEfforts } from "./catalog";
import { isFixedModel, orderExtraModels, pickerModelLabel, shortlistModels } from "./modelPicker";
import { agentUnavailable } from "../control/computerReview";
import { HARNESS_ACCOUNT, useSignInOn } from "../settings/signIn";
import { SignInAgainButton } from "../settings/SignInAgain";
import { AgentModelUpdate } from "../settings/AgentModelUpdate";
import "./agentSheet.css";

function whyNot(h: HarnessInfo): string {
  if (h.installed.state !== "available") return availabilityText(h.installed).detail ?? availabilityText(h.installed).label;
  if (h.integration === "terminal_only") return "Runs in a terminal only. Open it from Control › Terminal.";
  if (h.integration === "not_integrated") return "This version can’t manage it yet.";
  return availabilityText(h.capabilities.start_run).detail ?? "Can’t start work right now.";
}

function account(h: HarnessInfo, signInOn: string | null): string {
  switch (h.account.state) {
    case "signed_in":
      return ["Signed in", h.account.account_label, h.account.plan].filter(Boolean).join(" · ");
    case "signed_out":
      return signInOn && HARNESS_ACCOUNT[h.kind] ? `Not signed in on ${signInOn}` : `Not signed in. ${h.account.remedy}`;
    default:
      return `Sign-in unknown. ${h.account.reason}`;
  }
}

export function AgentSheet({
  open,
  onClose,
  project,
  value,
  onChange,
  mode: selectedMode,
  launchFlags = true,
  computerUse = false,
  fixedAgent = false,
  nextMessage = false,
  conversationTask,
  title = "Agent for this task",
  doneLabel = "Use this agent",
  note,
  compact = false,
  after,
}: {
  open: boolean;
  onClose: () => void;
  project: Project;
  value: AgentChoice;
  onChange: (a: AgentChoice) => void;
  mode?: import("../../protocol/types").TaskMode;
  /** Whether launch flags can be chosen here; the computer may not apply them. */
  launchFlags?: boolean;
  /**
   * For computer use: only agents ready to use the computer can be chosen,
   * Codex first, and it runs at the agent's own effort with no launch flags.
   */
  computerUse?: boolean;
  /** Only the model and effort change, as between a conversation's messages. */
  fixedAgent?: boolean;
  /** A chat's next reply never changes project defaults or saved usual choices. */
  nextMessage?: boolean;
  /** Saved history scopes provider choices to this conversation and computer. */
  conversationTask?: Task;
  title?: string;
  doneLabel?: string;
  /** Said under the title, such as when the choice takes effect. */
  note?: string;
  /**
   * The small picker over a suggestion's card: agent, model and effort only,
   * each pick shown at once, and `after` (More options) beneath them.
   */
  compact?: boolean;
  after?: ReactNode;
}) {
  const { ui, profile } = useApp();
  const harnesses = useHost((s) => s.harnesses);
  const savedRuns = useHost((s) => s.runs);
  const readChoices = () => conversationTask
    ? conversationMemory(conversationTask, savedRuns, readEffortMemory(profile.profileId, conversationTask.task_id))
    : readEffortMemory(profile.profileId);
  // Keep last known choices even when a later snapshot cannot fit older runs.
  // This is a history cache, never a project or task-type preference.
  useEffect(() => {
    if (conversationTask) saveEffortMemory(profile.profileId, readChoices(), conversationTask.task_id);
  }, [conversationTask, savedRuns, profile.profileId]);
  // A task on its own has no project yet to keep defaults for.
  const canSetDefaults = useHost((s) => (s.grants?.admin ?? false) && !!s.projects[project.project_id]);
  const link = useLink();
  const draftMode = useUi((s) => s.drafts[`${project.project_id}/${s.taskByProject[project.project_id] ?? "new"}`]?.mode ?? "work");
  const mode = selectedMode ?? draftMode;
  const run = useCommand();
  const [pick, setPick] = useState(value);
  const [asDefault, setAsDefault] = useState(false);
  const [effortChange, setEffortChange] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const wasOpen = useRef(false);
  const [allModels, setAllModels] = useState(false);
  const moreModelsId = useId();
  const shortModelGroup = useRef<HTMLDivElement>(null);
  const focusChosenModel = useRef(false);
  const [initialChoices] = useState(readChoices);
  const preferredChoices = useRef(initialChoices);
  const choices = useRef(initialChoices);
  const launches = useRef(new Map<string, AgentChoice["launchPolicy"]>());
  const launchSupported = useHost(s => !!s.capabilities?.features.includes(LAUNCH_POLICY_FEATURE)) || !launchFlags;

  useEffect(() => {
    if (open && !wasOpen.current) {
      choices.current = readChoices();
      preferredChoices.current = readChoices();
      setAllModels(false);
      keepEffort(choices.current, value);
      launches.current.clear();
      setPick({ ...value, launchPolicy: value.launchPolicy === undefined ? defaultLaunchPolicy(value.harness) : value.launchPolicy });
      setAsDefault(false);
      setEffortChange(null);
    }
    wasOpen.current = open;
  }, [open, value, profile.profileId]);

  const h = harnesses.find((x) => x.kind === pick.harness);
  const efforts = modelEfforts(h, pick.model);
  const shortModels = h ? shortlistModels(h, pick.model, preferredChoices.current) : [];
  const moreModels = orderExtraModels(h?.models.filter(m => !shortModels.some(s => s.id === m.id)) ?? []);
  const otherModels = moreModels.filter(m => !isFixedModel(m));
  const fixedModels = moreModels.filter(isFixedModel);
  const selectedFixed = shortModels.some(m => m.id === pick.model && isFixedModel(m));
  const signInOn = useSignInOn();
  const signInFor = h?.account.state === "signed_out" && signInOn ? HARNESS_ACCOUNT[h.kind] : undefined;
  const modeBlocker = (x: HarnessInfo) => {
    if (!nextMessage) return null;
    const support = availabilityText(modeSupport(x, "ask").availability);
    return support.ok ? null : support.detail ?? "Can’t hold a conversation on this computer.";
  };
  const choiceBlocker = agentChoiceBlocker(h, pick, signInOn) ?? (h ? modeBlocker(h) : null) ?? (computerUse && h ? agentUnavailable(h, signInOn) : null)
    ?? (pick.launchPolicy != null && !launchSupported ? LAUNCH_POLICY_UPDATE : null);
  const valid = !choiceBlocker && modelChoiceValid(h, pick.model, computerUse ? null : pick.effort);
  const flags = launchFlags && !computerUse ? launchChoices(pick.harness) : [];
  const chooseLaunch = (launchPolicy: AgentChoice["launchPolicy"]) => {
    launches.current.set(pick.harness, launchPolicy);
    setPick({ ...pick, launchPolicy });
  };
  const defaultModel = h?.models.find((m) => m.is_default);
  const suggested = h?.models.filter((m) => m.suggested_for_computer_use) ?? [];
  const isProjectDefault =
    project.default_harness === pick.harness && project.default_model === pick.model && project.default_effort === pick.effort;

  // On a card each valid pick shows at once; closing keeps it.
  useEffect(() => {
    if (!compact || !open || !valid) return;
    const out = computerUse ? { ...pick, effort: null, launchPolicy: null } : pick;
    if (out.harness === value.harness && out.model === value.model && out.effort === value.effort
      && out.launchPolicy === (computerUse ? value.launchPolicy ?? null : value.launchPolicy === undefined ? defaultLaunchPolicy(value.harness) : value.launchPolicy)) return;
    onChange(out);
  }, [compact, open, valid, pick, value, computerUse, onChange]);

  const managed = harnesses.filter((x) => x.integration === "managed");
  const usable = (x: HarnessInfo) => canStart(x) && x.account.state === "signed_in" && !modeBlocker(x) && !(computerUse && agentUnavailable(x));
  const unusable = (x: HarnessInfo) => (computerUse ? agentUnavailable(x) : null)
    ?? (!canStart(x) ? whyNot(x) : x.account.state !== "signed_in" ? account(x, signInOn) : modeBlocker(x) ?? account(x, signInOn));
  const chooseAgent = (x: HarnessInfo) => {
    if (x.kind === pick.harness) return;
    setAllModels(false);
    keepEffort(choices.current, pick);
    // History may have arrived while this sheet was open. Only fill providers
    // not already touched here; an explicit current selection always wins.
    const saved = readChoices();
    for (const kind of Object.keys(saved.models)) {
      if (Object.hasOwn(choices.current.models, kind)) continue;
      choices.current.models[kind] = saved.models[kind]!;
      for (const [key, effort] of Object.entries(saved.efforts)) {
        if (key.startsWith(`${kind}/`)) choices.current.efforts[key] = effort;
      }
    }
    const priorModel = choices.current.models[x.kind];
    const removed = !!priorModel && (x.capabilities.model_selection.state !== "available" || !x.models.some(m => m.id === priorModel));
    const model = removed ? null : rememberedModel(profile.profileId, x, choices.current);
    const restored = switchEffort(profile.profileId, x, model, pick, choices.current);
    const effort = removed ? null : restored.effort;
    setEffortChange(removed
      ? `Your previous model (${priorModel}) is unavailable for ${x.display_name}. Using the agent’s default model and effort.`
      : conversationTask && !Object.hasOwn(choices.current.models, x.kind)
      ? `No earlier model choice is available for ${x.display_name} in this chat. Using the agent’s default model.`
      : restored.notice);
    launches.current.set(pick.harness, pick.launchPolicy);
    setPick({ harness: x.kind, model, effort, launchPolicy: launches.current.has(x.kind) ? launches.current.get(x.kind) : defaultLaunchPolicy(x.kind) });
  };

  // A choice from the full list moves into the short group. Carry keyboard
  // focus with it rather than leaving focus on a button that was removed.
  useEffect(() => {
    if (!focusChosenModel.current) return;
    focusChosenModel.current = false;
    shortModelGroup.current?.querySelector<HTMLButtonElement>('[aria-pressed="true"]')?.focus();
  }, [pick.model]);

  const chooseModel = (model: string | null) => {
    focusChosenModel.current = moreModels.some(m => m.id === model);
    keepEffort(choices.current, pick);
    const { effort, notice } = switchEffort(profile.profileId, h, model, pick, choices.current);
    setEffortChange(notice);
    setPick({ ...pick, model, effort });
  };

  const done = async () => {
    if (!valid) return;
    if (asDefault && !isProjectDefault) {
      setSaving(true);
      const receipt = await run(
        { command: "update_project_defaults", project_id: project.project_id, harness: pick.harness, model: pick.model, effort: pick.effort },
        { label: "Set project defaults", success: `New tasks in ${project.name} will use this` },
      );
      setSaving(false);
      if (!receipt) return;
    }
    keepEffort(choices.current, pick);
    if (!nextMessage) saveEffortMemory(profile.profileId, choices.current);
    onChange(computerUse ? { ...pick, effort: null, launchPolicy: null } : pick);
    onClose();
  };

  return (
    <Sheet
      className={compact ? "agent-sheet agent-sheet--compact" : "agent-sheet"}
      open={open}
      onClose={onClose}
      title={title}
      subtitle={compact && note ? note : fixedAgent ? h?.display_name ?? project.name : project.name}
      size={compact ? "auto" : "tall"}
      footer={
        <div className="talk-agents__footer">
          <Button variant="primary" block disabled={!valid} pending={saving} onClick={() => void done()}>
            {doneLabel}
          </Button>
        </div>
      }
    >
      {note && !compact && <p className="fine agent-sheet__note">{note}</p>}
      {choiceBlocker && <Notice tone="warn" action={signInFor ? <SignInAgainButton accountId={signInFor} leave={onClose} /> : undefined}>{choiceBlocker}</Notice>}
      {!fixedAgent && <Section title="Agent">
        <div className="talk-agents__chips" role="group" aria-label="Agent">
          {managed.map((x) => (
            <Chip key={x.kind} selected={x.kind === pick.harness} disabled={!usable(x)} onClick={() => chooseAgent(x)}>
              {x.display_name}
            </Chip>
          ))}
        </div>
        {managed.filter((x) => !usable(x)).map((x) => <p key={x.kind} className="fine">{`${x.display_name}: ${unusable(x)}`}</p>)}
      </Section>}
      {h && (
        <Section title="Model" action={moreModels.length > 0 && <button type="button" className="agent-sheet__toggle" aria-expanded={allModels} aria-controls={moreModelsId} onClick={() => setAllModels(v => !v)}>{allModels ? "Fewer models" : "All models"}</button>}>
          {h.capabilities.model_selection.state === "available" && h.models.length > 0 ? (
            <>
              <div ref={shortModelGroup} className="talk-agents__chips" role="group" aria-label="Model">
                <Chip selected={pick.model === null} onClick={() => chooseModel(null)}>Agent’s default</Chip>
                {shortModels.map(m => <Chip key={m.id} selected={pick.model === m.id} onClick={() => chooseModel(m.id)}>
                  {pickerModelLabel(m, h.display_name)}
                </Chip>)}
              </div>
              <div id={moreModelsId} hidden={!allModels} className="agent-sheet__all-models" role="group" aria-label="More models">
                {otherModels.length > 0 && <div className="talk-agents__chips">
                  {otherModels.map(m => <Chip key={m.id} selected={pick.model === m.id} onClick={() => chooseModel(m.id)}>
                    {pickerModelLabel(m, h.display_name)}
                  </Chip>)}
                </div>}
                {fixedModels.length > 0 && <>
                  <p className="fine">Fixed versions stay on that version when a newer one comes out.</p>
                  <div className="talk-agents__chips">
                    {fixedModels.map(m => <Chip key={m.id} selected={pick.model === m.id} onClick={() => chooseModel(m.id)}>
                      {pickerModelLabel(m, h.display_name)}
                    </Chip>)}
                  </div>
                </>}
              </div>
            </>
          ) : (
            <Notice tone="unknown">The computer hasn’t reported model choices. The agent’s default is still available.</Notice>
          )}
          {defaultModel && !pick.model && <p className="fine">Default on the computer: {pickerModelLabel(defaultModel, h.display_name)}.</p>}
          {computerUse && suggested.length > 0 && <p className="fine">Suggested for computer use: {suggested.map(m => pickerModelLabel(m, h.display_name)).join(", ")}.</p>}
        </Section>
      )}
      {h && !computerUse && (
        <Section title="Effort">
          {effortChange && <Notice>{effortChange}</Notice>}
          {h.capabilities.effort_selection.state === "available" && efforts.length > 0 ? (
            <div className="talk-agents__chips" role="group" aria-label="Effort">
              <Chip selected={pick.effort === null} onClick={() => { setPick({ ...pick, effort: null }); setEffortChange(null); }}>
                Agent’s default
              </Chip>
              {efforts.map((e) => (
                <Chip key={e.id} selected={pick.effort === e.id} onClick={() => { setPick({ ...pick, effort: e.id }); setEffortChange(null); }}>
                  {e.label}
                </Chip>
              ))}
            </div>
          ) : (
            <p className="fine">No effort choices reported for this model. The agent uses its own default.</p>
          )}
        </Section>
      )}
      {selectedFixed && (!allModels || fixedModels.length === 0) && <p className="fine">Fixed versions stay on that version when a newer one comes out.</p>}
      {h && open && <AgentModelUpdate harness={h} leave={onClose} />}
      {h?.catalog && !compact && <p className="fine">Model choices · Checked <Age at={h.catalog.checked_at} suffix=" ago" />{link.state !== "online" ? " · Offline; choices may have changed" : ""}</p>}
      {flags.length > 0 && <Section title="Permission prompts">
        <div className="talk-agents__chips" role="group" aria-label="Permission prompts">
          {flags.map(f => <Chip key={f.value} selected={pick.launchPolicy === f.value} onClick={() => chooseLaunch(f.value)}>{f.label}</Chip>)}
        </div>
        {mode === "work" ? <>
          {!launchRisk(pick.harness, pick.launchPolicy, mode) && <p className="fine">{flags.find(f => f.value === pick.launchPolicy)?.detail}</p>}
          <LaunchLine className="fine" harness={pick.harness} policy={pick.launchPolicy} mode={mode} label={false} />
        </> : <p className="fine">{launchSummary(pick.harness, pick.launchPolicy, mode)}</p>}
        <p className="fine">Saved with this task, including continuations. Device permissions, Stop and private-mode boundaries still apply.</p>
        <Button variant="quiet" onClick={() => { onClose(); ui.push({ screen: "settings", section: "launch-flags" }); }}>Defaults in Settings</Button>
      </Section>}
          {canSetDefaults && !nextMessage && !compact && !computerUse && !fixedAgent && !isProjectDefault && link.state === "online" && (
            <Chip selected={asDefault} onClick={() => setAsDefault((v) => !v)}>
              Also use this agent, model and effort for new tasks in {project.name}
            </Chip>
          )}
      {after}
      {!fixedAgent && !compact && <details className="agent-sheet__more"><summary>Other agents</summary><div className="group">{harnesses.filter((h) => h.integration !== "managed").map((h) => <ListRow key={h.kind} icon={Bot} title={h.display_name} subtitle={h.installed.state === "available" ? "Installed, not supported by this version of the app yet" : "Not found on this computer"} />)}</div></details>}
    </Sheet>
  );
}
