// SPDX-License-Identifier: Apache-2.0
//
// Turn into a task: a conversation that has come to something worth doing
// becomes a task. Where the computer can tell a chat how its work ended, it
// is a new task, made as Tasks would make it (the person's usual agent, its
// own branch and pull request where GitHub is on), told what was said, and
// the chat stays. Elsewhere the conversation carries on in Work, with the
// same agent in the same session, and leaves Talk's conversations for Work's
// tasks; on a computer that holds a conversation to its access, the task
// keeps it, with its own project to change.

import { Hammer } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "../../design/Button";
import { TextArea } from "../../design/Field";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { useApp, useHost } from "../../app/context";
import type { AgentChoice } from "../../app/ui";
import { usualAgent } from "../../app/launchMemory";
import { AgentChip } from "./AgentChip";
import { AgentSheet } from "./AgentSheet";
import { agentLabel } from "./intent";
import type { ConversationAccess, Project } from "../../protocol/types";
import { carriedAccess, describeAccess } from "./access";

export const INTO_TASK_TEXT = "Go ahead with what we discussed.";

export function IntoTaskSheet({
  open,
  onClose,
  project,
  agent,
  access,
  keeps,
  initial,
  blocked,
  onOpenBlocker,
  pending,
  onStart,
  onPick,
}: {
  open: boolean;
  onClose: () => void;
  project: Project;
  /** The conversation's agent, by name. */
  agent: string;
  /** What the computer holds the conversation to, if it holds it. */
  access: ConversationAccess | null;
  /** A new task the chat stays beside: what Start makes, and why it opens
   * Review instead, when it does. */
  keeps?: { line: string; agent: string; review: string | null } | null;
  /** The words the sheet opens with. */
  initial: string;
  /** Why it can't start now, if it can't. */
  blocked: string | null;
  /** Opens the task that holds the project, when that is what blocks it. */
  onOpenBlocker?: () => void;
  pending: boolean;
  onStart: (text: string, agent?: AgentChoice) => void;
  /** The agent the person picked here, or null back to their usual, so what
   * the sheet says it starts is worked out for that agent. */
  onPick?: (agent: AgentChoice | null) => void;
}) {
  const { profile } = useApp();
  const projects = useHost((s) => s.projects);
  const harnesses = useHost((s) => s.harnesses);
  const initialAgent = keeps ? usualAgent(profile.profileId, project, "change", harnesses)?.agent ?? null : null;
  const [choice, setChoice] = useState<AgentChoice | null>(null);
  const [picking, setPicking] = useState(false);
  const picked = choice ?? initialAgent;
  const [text, setText] = useState(initial);
  const wasOpen = useRef(false);
  useEffect(() => {
    if (open && !wasOpen.current) {
      setText(initial);
      setChoice(null);
      onPick?.(null);
      setPicking(false);
    }
    wasOpen.current = open;
  }, [open, initial]);
  return (
    // What was typed outlasts a tapped notification opening another screen.
    <><Sheet
      open={open}
      keepWhenHidden
      onClose={onClose}
      title="Turn into a task"
      subtitle={project.name}
      footer={
        <div className="talk-agents__footer">
          <Button variant="primary" block icon={Hammer} pending={pending} disabled={!text.trim() || blocked !== null} onClick={() => onStart(text, keeps ? picked ?? undefined : undefined)}>
            {keeps?.review ? "Review the task" : "Start the task"}
          </Button>
        </div>
      }
    >
      <div className="into-task">
        {keeps ? <>
          <p className="fine">A new task starts from what was said here, and this chat stays. When the task ends, the chat hears how.</p>
          <p className="fine">{keeps.line}.</p>
          {picked ? <AgentChip label={agentLabel(harnesses, picked)} lead="Agent for this task:" onClick={() => setPicking(true)} />
            : <p className="fine">{keeps.agent}</p>}
          {keeps.review && <p className="fine">{keeps.review}</p>}
        </> : <p className="fine">
          {agent} carries on in this conversation’s session, in Work mode, and it moves to Tasks.{" "}
          {access ? `It keeps this conversation’s access, and may change ${project.name}. ${describeAccess(carriedAccess(access, project.project_id), project, projects)}`
            : `Like any task in Work mode, it can change files and run commands in ${project.name} and reach the internet, as far as ${agent}’s own permissions allow.`}
        </p>}
        <TextArea label="What should it do?" value={text} onChange={setText} maxRows={8} autoCapitalize="sentences" spellCheck />
        {blocked && (
          <Notice tone="warn" action={onOpenBlocker && <Button size="sm" variant="secondary" aria-label="Open the blocking task" onClick={onOpenBlocker}>Open</Button>}>
            {blocked}
          </Notice>
        )}
      </div>
    </Sheet>
    {keeps && picked && <AgentSheet
      open={open && picking}
      onClose={() => setPicking(false)}
      project={project}
      value={picked}
      onChange={(a) => { setChoice(a); onPick?.(a); }}
      mode="work"
      compact
      launchFlags={false}
      doneLabel="Done"
    />}
    </>
  );
}
