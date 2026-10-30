// SPDX-License-Identifier: Apache-2.0
//
// Which agent a new task uses. Only agents this build can manage are
// selectable; the others stay listed with the reason and the way forward, so
// nothing is hidden and nothing is offered that would not work.

import { defaultLaunchPolicy, launchChoices, launchRisk, launchSummary, LAUNCH_POLICY_FEATURE, LAUNCH_POLICY_UPDATE } from "../../app/launchPolicy";
import { LaunchLine } from "./LaunchLine";
import { keepEffort, readEffortMemory, saveEffortMemory, rememberedModel, switchEffort } from "./effortMemory";
import { Bot, Check, SquareTerminal } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useApp, useCommand, useHost, useLink, useUi } from "../../app/context";
import type { AgentChoice } from "../../app/ui";
import { Button } from "../../design/Button";
import { Chip } from "../../design/Chip";
import { ListRow, Section } from "../../design/ListRow";
import { availabilityText } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import type { HarnessInfo, Project } from "../../protocol/types";
import { agentChoiceBlocker, canStart } from "./intent";

import { Age } from "../../design/Age";
import { Notice } from "../../design/Notice";
import { modelChoiceValid, modelEfforts } from "./catalog";
import { agentUnavailable } from "../control/computerReview";
import "./agentSheet.css";

function whyNot(h: HarnessInfo): string {
  if (h.installed.state !== "available") return availabilityText(h.installed).detail ?? availabilityText(h.installed).label;
  if (h.integration === "terminal_only") return "Runs in a terminal only. Open it from Control › Terminal.";
  if (h.integration === "not_integrated") return "This version can’t manage it yet.";
  return availabilityText(h.capabilities.start_run).detail ?? "Can’t start work right now.";
}

function account(h: HarnessInfo): string {
  switch (h.account.state) {
    case "signed_in":
      return ["Signed in", h.account.account_label, h.account.plan].filter(Boolean).join(" · ");
    case "signed_out":
      return `Not signed in. ${h.account.remedy}`;
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
  title = "Agent for this task",
  doneLabel = "Use this agent",
  note,
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
  title?: string;
  doneLabel?: string;
  /** Said under the title, such as when the choice takes effect. */
  note?: string;
}) {
  const { ui, profile } = useApp();
  const harnesses = useHost((s) => s.harnesses);
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
  const choices = useRef(readEffortMemory(profile.profileId));
  const launches = useRef(new Map<string, AgentChoice["launchPolicy"]>());
  const launchSupported = useHost(s => !!s.capabilities?.features.includes(LAUNCH_POLICY_FEATURE)) || !launchFlags;

  useEffect(() => {
    if (open && !wasOpen.current) {
      choices.current = readEffortMemory(profile.profileId);
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
  const choiceBlocker = agentChoiceBlocker(h, pick) ?? (computerUse && h ? agentUnavailable(h) : null)
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

  const chooseModel = (model: string | null) => {
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
    saveEffortMemory(profile.profileId, choices.current);
    onChange(computerUse ? { ...pick, effort: null, launchPolicy: null } : pick);
    onClose();
  };

  return (
    <Sheet
      className="agent-sheet"
      open={open}
      onClose={onClose}
      title={title}
      subtitle={fixedAgent ? h?.display_name ?? project.name : project.name}
      size="tall"
      footer={
        <div className="talk-agents__footer">
          <Button variant="primary" block disabled={!valid} pending={saving} onClick={() => void done()}>
            {doneLabel}
          </Button>
        </div>
      }
    >
      {note && <p className="fine">{note}</p>}
      {choiceBlocker && <Notice tone="warn">{choiceBlocker}</Notice>}
      {!fixedAgent && <Section title="Agent">
        <div className="group" role="group" aria-label="Agent">
          {harnesses.filter((h) => h.integration === "managed").map((x) => {
            const unable = computerUse ? agentUnavailable(x) : null;
            const ok = canStart(x) && x.account.state === "signed_in" && !unable;
            const selected = x.kind === pick.harness;
            return (
              <ListRow
                key={x.kind}
                icon={x.integration === "terminal_only" ? SquareTerminal : Bot}
                title={
                  <>
                    {x.display_name}
                    {x.native_version && <span className="talk-agents__version"> {x.native_version}</span>}
                  </>
                }
                subtitle={ok ? account(x) : unable ?? (x.integration === "managed" && x.installed.state === "available" ? account(x) : whyNot(x))}
                selected={selected}
                className={ok ? undefined : "talk-agents__off"}
                trailing={selected ? <Check className="row__check" aria-label="Chosen" /> : undefined}
                onClick={ok ? () => {
                  keepEffort(choices.current, pick);
                  const model = rememberedModel(profile.profileId, x, choices.current);
                  const { effort, notice } = switchEffort(profile.profileId, x, model, pick, choices.current);
                  setEffortChange(notice);
                  launches.current.set(pick.harness, pick.launchPolicy);
                  setPick({ harness: x.kind, model, effort, launchPolicy: launches.current.has(x.kind) ? launches.current.get(x.kind) : defaultLaunchPolicy(x.kind) });
                } : undefined}
              />
            );
          })}
        </div>
        {computerUse && <p className="fine">Only agents that can use the screen are offered.</p>}
      </Section>}
      {h?.catalog && <p className="fine">Model choices · Checked <Age at={h.catalog.checked_at} suffix=" ago" />{link.state !== "online" ? " · Offline; choices may have changed" : ""}</p>}
      {h && (
        <Section title="Model">
          {h.capabilities.model_selection.state === "available" && h.models.length > 0 ? (
            <div className="talk-agents__chips" role="group" aria-label="Model">
              <Chip selected={pick.model === null} onClick={() => chooseModel(null)}>Agent’s default</Chip>
              {h.models.map((m) => {
                const selected = pick.model === m.id;
                return (
                  // "Agent’s default" says which model that is; one default is enough.
                  <Chip key={m.id} selected={selected} onClick={() => chooseModel(m.id)}>
                    {m.label}
                  </Chip>
                );
              })}
            </div>
          ) : (
            <Notice tone="unknown">The computer hasn’t reported model choices. The agent’s default is still available.</Notice>
          )}
          {defaultModel && !pick.model && <p className="fine">Uses {h.display_name}’s default on the computer, currently {defaultModel.label}.</p>}
          {computerUse && suggested.length > 0 && <p className="fine">Suggested for computer use: {suggested.map((m) => m.label).join(", ")}.</p>}
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
          <p className="fine">The agent reports what it actually used; Chat shows both if they differ.</p>
        </Section>
      )}
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
          {canSetDefaults && !computerUse && !fixedAgent && !isProjectDefault && link.state === "online" && (
            <Chip selected={asDefault} onClick={() => setAsDefault((v) => !v)}>
              Also use this agent, model and effort for new tasks in {project.name}
            </Chip>
          )}
      {!fixedAgent && <details className="agent-sheet__more"><summary>Other agents</summary><div className="group">{harnesses.filter((h) => h.integration !== "managed").map((h) => <ListRow key={h.kind} icon={Bot} title={h.display_name} subtitle={h.installed.state === "available" ? "Installed, not supported by this version of the app yet" : "Not found on this computer"} />)}</div></details>}
    </Sheet>
  );
}
