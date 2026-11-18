// SPDX-License-Identifier: Apache-2.0
//
// The chip naming the agent, model and effort a piece of work starts with.
// Tapping it opens the agent picker. Shared by a chat's work cards and the
// "Turn into a task" sheet, so the choice works the same way in both.

import { Bot, ChevronDown } from "lucide-react";
import { Chip } from "../../design/Chip";
import "./agentChip.css";

/** Only shorten the agent, never the model or effort. The full name remains
 * the button's accessible label. Codex needn't be named twice. */
export function shortAgentLabel(label: string): string {
  return label.replace(/^Claude Code · /, "Claude · ").replace(/^Codex · (?=[^·]*Codex)/i, "");
}

export function AgentChip({ label, lead, onClick }: { label: string; lead: string; onClick: () => void }) {
  return (
    <Chip icon={Bot} className="composer__chip work-card__chip" onClick={onClick} aria-haspopup="dialog" aria-label={`${lead} ${label}`}>
      <span className="work-card__chip-text" aria-hidden>{shortAgentLabel(label)}</span>
      <ChevronDown className="composer__chip-chev" aria-hidden />
    </Chip>
  );
}
