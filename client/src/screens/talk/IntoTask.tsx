// SPDX-License-Identifier: Apache-2.0
//
// Turn into a task: a conversation that has come to something worth doing
// carries on in Work, with the same agent in the same session, so it can
// change files. It then leaves Talk's conversations for Work's tasks. On a
// computer that holds a conversation to its access, the task keeps it, with
// its own project to change; the sheet says all of what it may then reach.

import { Hammer } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "../../design/Button";
import { TextArea } from "../../design/Field";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { useHost } from "../../app/context";
import type { ConversationAccess, Project } from "../../protocol/types";
import { carriedAccess, describeAccess } from "./access";

export const INTO_TASK_TEXT = "Go ahead with what we discussed.";

export function IntoTaskSheet({
  open,
  onClose,
  project,
  agent,
  access,
  initial,
  blocked,
  onOpenBlocker,
  pending,
  onStart,
}: {
  open: boolean;
  onClose: () => void;
  project: Project;
  /** The conversation's agent, by name. */
  agent: string;
  /** What the computer holds the conversation to, if it holds it. */
  access: ConversationAccess | null;
  /** The words the sheet opens with. */
  initial: string;
  /** Why it can't start now, if it can't. */
  blocked: string | null;
  /** Opens the task that holds the project, when that is what blocks it. */
  onOpenBlocker?: () => void;
  pending: boolean;
  onStart: (text: string) => void;
}) {
  const projects = useHost((s) => s.projects);
  const [text, setText] = useState(initial);
  const wasOpen = useRef(false);
  useEffect(() => {
    if (open && !wasOpen.current) setText(initial);
    wasOpen.current = open;
  }, [open, initial]);
  return (
    <Sheet
      open={open}
      onClose={onClose}
      title="Turn into a task"
      subtitle={project.name}
      footer={
        <div className="talk-agents__footer">
          <Button variant="primary" block icon={Hammer} pending={pending} disabled={!text.trim() || blocked !== null} onClick={() => onStart(text)}>
            Start the task
          </Button>
        </div>
      }
    >
      <div className="into-task">
        <p className="fine">
          {agent} carries on in this conversation’s session, in Work mode, and it moves to Tasks.{" "}
          {access ? `It keeps this conversation’s access, and may change ${project.name}. ${describeAccess(carriedAccess(access, project.project_id), project, projects)}`
            : `Like any task in Work mode, it can change files and run commands in ${project.name} and reach the internet, as far as ${agent}’s own permissions allow.`}
        </p>
        <TextArea label="What should it do?" value={text} onChange={setText} maxRows={8} autoCapitalize="sentences" spellCheck />
        {blocked && (
          <Notice tone="warn" action={onOpenBlocker && <Button size="sm" variant="secondary" aria-label="Open the blocking task" onClick={onOpenBlocker}>Open</Button>}>
            {blocked}
          </Notice>
        )}
      </div>
    </Sheet>
  );
}
