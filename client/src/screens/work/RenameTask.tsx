// SPDX-License-Identifier: Apache-2.0
//
// Renaming a task or a conversation: its title, saved on this device until
// the computer has it, and sent against the title it replaces.

import { useState } from "react";
import { useApp, useCommand, useHost } from "../../app/context";
import { Button } from "../../design/Button";
import { TextField } from "../../design/Field";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import type { Task } from "../../protocol/types";
import { useLocalText } from "./local";
import { CommandFeedback, useCanWork, useUnconfirmedCommand } from "./shared";

/** The title field and Save title, for a sheet that renames `task`. */
export function RenameFields({ task, onDone }: { task: Task; onDone: () => void }) {
  const { profile } = useApp();
  const send = useCommand();
  const can = useCanWork();
  const uncertain = useUnconfirmedCommand((c) => "task_id" in c && c.task_id === task.task_id);
  const current = useHost((s) => s.tasks[task.task_id]);
  const [title, setTitle, saved] = useLocalText(`${profile.profileId}.rename.${task.task_id}`, task.title);
  const [pending, setPending] = useState(false);
  const noun = task.conversation ? "conversation" : "task";
  const save = async () => {
    setPending(true);
    const r = await send(
      { command: "rename_task", task_id: task.task_id, title: title.trim(), expected_title: task.title },
      {
        toastErrors: false,
        label: `Rename ${noun}`,
        // A computer that predates expected_title fences by this.
        expectedRevision: task.revision,
      },
    );
    setPending(false);
    if (r) onDone();
  };
  return (
    <>
      {/* Agent steps change the task all the time; a rename only needs the title it replaces. */}
      {current && current.title !== task.title && (
        <Notice tone="unknown">
          This {noun} is now called “{current.title}” on the computer. Close this sheet and rename it again if you still want to.
        </Notice>
      )}
      <TextField label={task.conversation ? "Conversation title" : "Task title"} value={title} onChange={setTitle} maxLength={240} />
      {!saved && <Notice tone="stop">Not saved on this device. Keep this sheet open.</Notice>}
      <Button variant="primary" pending={pending} disabled={!can || uncertain || !title.trim() || current?.title !== task.title} onClick={() => void save()}>
        Save title
      </Button>
    </>
  );
}

/** Rename, on its own, for the task or conversation open in Talk. */
export function RenameSheet({ task: opened, onClose }: { task: Task; onClose: () => void }) {
  // The title as it was when the sheet opened, which the rename replaces.
  const [task] = useState(opened);
  return (
    <Sheet open onClose={onClose} className="work-sheet" title={task.conversation ? "Rename conversation" : "Rename task"} subtitle={task.title}>
      <div className="work-stack">
        <RenameFields task={task} onDone={onClose} />
        <CommandFeedback match={(c) => "task_id" in c && c.task_id === task.task_id && c.command === "rename_task"} />
      </div>
    </Sheet>
  );
}
