// SPDX-License-Identifier: Apache-2.0
//
// Disable in Øphio (D-071) in Library: the choice on a skill, agent or
// agent file, the Disabled section that holds what is disabled, apart from
// everything agents are given, and the line that says Library is still
// checking the computer.

import { Ban, Bot, Eye, EyeOff, FileText, Sparkles, type LucideIcon } from "lucide-react";
import { useApp, useLink } from "../../app/context";
import { DISABLE_ABOUT, kindLabel, useDisabled, type DisableTarget } from "../../app/disabled";
import { Button } from "../../design/Button";
import { ListRow, Section } from "../../design/ListRow";
import type { Disable, DisabledItem, DisabledKind, AgentFileGroup } from "../../protocol/types";
import { groupIcon } from "./shared";

function iconFor(item: { kind: DisabledKind; group?: AgentFileGroup | null }): LucideIcon {
  if (item.group) return groupIcon[item.group];
  if (item.kind === "skill") return Sparkles;
  if (item.kind === "agent") return Bot;
  return FileText;
}

/**
 * Disable in Øphio, or Enable in Øphio, for one entry or file, with what it
 * does; or why it can't be. Nothing shows for a computer that can't.
 */
export function DisableControl({ target, disable }: { target: DisableTarget; disable: Disable | undefined }) {
  const d = useDisabled();
  const online = useLink().state === "online";
  if (!disable || !d.supported) return null;
  if (disable.state === "cannot") {
    return (
      <p className="lib-disable lib-disable--cannot fine">
        <Ban aria-hidden className="lib-disable__mark" />
        <span><strong>Can’t be disabled in Øphio.</strong> {disable.reason}</span>
      </p>
    );
  }
  const off = d.isDisabled(target.key ?? target.host_path, disable.disabled, !!target.key);
  return (
    <div className="lib-disable">
      <Button
        variant="secondary"
        icon={off ? Eye : EyeOff}
        disabled={!d.canChange || !online}
        onClick={() => d.set(target, !off)}
      >
        {off ? "Enable in Øphio" : "Disable in Øphio"}
      </Button>
      <p className="fine" aria-live="polite">
        {off
          ? "Disabled in Øphio: agents that Øphio runs don’t see it. The file isn’t changed."
          : DISABLE_ABOUT}
        {!d.canChange && " This device isn’t allowed to change Library."}
      </p>
    </div>
  );
}

/** What the Disabled section holds: with a project chosen, that project's
 * and those that apply everywhere; for one kind of agent file, that kind. */
export function shownDisabled(items: DisabledItem[], projectId?: string | null, only?: AgentFileGroup): DisabledItem[] {
  return items.filter(
    (i) =>
      (!projectId || !i.project_id || i.project_id === projectId) &&
      (!only || (i.group ? i.group === only : (only === "skills" && i.kind === "skill") || (only === "agents" && i.kind === "agent"))),
  );
}

/**
 * Everything disabled in Øphio, below and apart from what agents are given,
 * each with its kind and a way back. `only` narrows it to one kind of agent
 * file; with a project chosen, it holds that project's and those that apply
 * everywhere.
 */
export function DisabledSection({ projectId, only }: { projectId?: string | null; only?: AgentFileGroup }) {
  const d = useDisabled();
  const { ui, profile } = useApp();
  const online = useLink().state === "online";
  if (!d.supported || !d.allowed) return null;
  const items = shownDisabled(d.items, projectId, only);
  if (!items.length) return null;
  const open = (item: DisabledItem) =>
    item.library && ui.push({ screen: "file", rootId: item.library.root_id, path: item.library.path });
  return (
    <Section title="Disabled in Øphio" id="lib-disabled" className="lib-disabled">
      <p className="lib-meta">
        Kept from every agent Øphio runs. Their files aren’t changed, and Claude Code and Codex used outside Øphio still have them.
      </p>
      <div className="group lib-disabled__group">
        {items.map((item) => (
          <div className="lib-disabled__row" key={item.host_path}>
            <ListRow
              icon={iconFor(item)}
              title={item.name}
              subtitle={
                <>
                  <span>{kindLabel(item)} · {item.scope}</span>
                  {!item.found && <span className="lib-disabled__gone">Not on {profile.name} now. It stays disabled if it comes back.</span>}
                </>
              }
              onClick={item.library ? () => open(item) : undefined}
              chevron={!!item.library}
              ariaLabel={item.library ? `Open ${item.name}` : undefined}
            />
            <Button
              size="sm"
              variant="secondary"
              icon={Eye}
              disabled={!d.canChange || !online}
              onClick={() => d.set(item, false)}
              aria-label={`Enable ${item.name} in Øphio`}
            >
              Enable
            </Button>
          </div>
        ))}
      </div>
    </Section>
  );
}

/**
 * Library is reading the computer: a spinner and a line on the first read,
 * and a small line beside what is already shown on a later one.
 */
export function Checking({ refreshing, what = "agents, skills and files" }: { refreshing?: boolean; what?: string }) {
  return refreshing ? (
    <p className="lib-checking lib-checking--inline" role="status" aria-live="polite">
      <span className="stateline__spinner" aria-hidden />
      <span>Checking again…</span>
    </p>
  ) : (
    <div className="lib-checking" role="status" aria-live="polite" aria-busy="true">
      <span className="lib-checking__spinner" aria-hidden />
      <span>Checking for {what}…</span>
    </div>
  );
}
