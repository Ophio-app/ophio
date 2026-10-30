// SPDX-License-Identifier: Apache-2.0
//
// The checklist's row, right under New in Chat and in Tasks: what is left
// to do and what an agent waits on, or Request checklist while the project
// has none, which starts a task from the built-in checklist skill.

import { useState, useSyncExternalStore } from "react";
import { ListChecks, ListPlus } from "lucide-react";
import { CHECKLIST_SKILL, luminance, REQUEST_TEXT, useChecklistSummary } from "../../app/checklist";
import { describeError, useApp, useHost } from "../../app/context";
import { LAUNCHERS_FEATURE, launchStart, setDraftStart } from "../../app/launchers";
import { Button } from "../../design/Button";
import { ListRow } from "../../design/ListRow";
import { toast } from "../../design/Toast";
import type { ChecklistSummary, Project } from "../../protocol/types";
import "./row.css";

/** Starts a new task in `project` from the checklist skill, asking for one,
 * and opens it in Chat to send. */
export function useRequestChecklist(project: Project) {
  const { ui, session, profile } = useApp();
  const launchers = useHost((s) => !!s.capabilities?.features.includes(LAUNCHERS_FEATURE));
  const [busy, setBusy] = useState(false);
  const pid = project.project_id;
  const request = async () => {
    setBusy(true);
    try {
      // Without the skill, the words alone still ask for one.
      const catalog = launchers ? await session.query({ kind: "catalog", project_id: pid }).then((r) => r.catalog, () => null) : null;
      const skill = catalog?.entries.find((e) => e.kind === "skill" && e.source === "ophio_setup" && e.id === CHECKLIST_SKILL);
      if (skill) setDraftStart(ui, pid, launchStart(skill));
      if (!ui.draft(pid, null)?.text.trim()) ui.saveDraft(pid, null, { text: REQUEST_TEXT });
      ui.openTask(pid, null);
    } catch (e) {
      toast(describeError(e, profile.name), { tone: "stop" });
    } finally {
      setBusy(false);
    }
  };
  return { request, busy };
}

export function RequestChecklist({ project }: { project: Project }) {
  const { request, busy } = useRequestChecklist(project);
  return <Button variant="primary" icon={ListPlus} pending={busy} onClick={() => void request()}>Request checklist</Button>;
}

/** A screen shorter than this, in lines of its text, is short. */
const SHORT_REM = 36;
const short = () => {
  const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
  return window.innerHeight < SHORT_REM * rem;
};
const resized = (then: () => void) => {
  window.addEventListener("resize", then);
  return () => window.removeEventListener("resize", then);
};

/**
 * Whether the screen is short for its text, as a phone on its side or with
 * large text is. There Tasks shows what agents ask right now first, and the
 * checklist's row follows it instead of sitting under New.
 */
export function useShortScreen(): boolean {
  return useSyncExternalStore(resized, short);
}

/** What the row says about a checklist: what is left, what stops work and what waits on the person. */
export function summaryWords(c: ChecklistSummary): string {
  if (c.open === 0) return `All ${c.done} done`;
  const parts = [`${c.open} to do`];
  if (c.next) parts.push(`next: ${c.next}`);
  if (c.blockers) parts.push(c.blockers === 1 ? "1 blocker" : `${c.blockers} blockers`);
  if (c.waiting) parts.push(c.waiting === 1 ? "an agent waits on one" : `agents wait on ${c.waiting}`);
  if (c.unanswered) parts.push(c.unanswered === 1 ? "a question asked" : `${c.unanswered} questions asked`);
  return parts.join(" · ");
}

/**
 * The checklist's row under New. Absent while the computer doesn't keep
 * checklists or this phone may not see tasks; until the computer has said
 * whether there is one, nothing, so the row doesn't flip.
 */
export function ChecklistRow({ project }: { project: Project }) {
  const { ui } = useApp();
  const summary = useChecklistSummary(project.project_id);
  const { request, busy } = useRequestChecklist(project);
  if (summary === undefined) return null;
  if (summary === null) {
    return (
      <ListRow icon={ListPlus} title="Request checklist" disabled={busy}
        subtitle="Everything you have to do yourself here, step by step"
        onClick={() => void request()} />
    );
  }
  // The brand's colour tints the icon only where it reads in both themes.
  const l = luminance(summary.accent);
  const accent = l !== null && l > 0.07 && l < 0.4 ? summary.accent : undefined;
  return (
    <ListRow
      className={`checklist-row${summary.waiting || summary.blockers ? " checklist-row--waiting" : ""}${summary.blockers ? " checklist-row--blocker" : ""}`}
      leading={<span className="row__icon checklist-row__icon" aria-hidden style={accent ? { color: accent } : undefined}><ListChecks strokeWidth={2} /></span>}
      title={summary.title}
      subtitle={summaryWords(summary)}
      chevron
      onClick={() => ui.openChecklist(project.project_id)}
    />
  );
}
