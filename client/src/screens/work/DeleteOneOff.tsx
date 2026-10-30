// SPDX-License-Identifier: Apache-2.0
import { useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import {
  describeError,
  useApp,
  useCommand,
  useHost,
  useLink,
} from "../../app/context";
import { Button } from "../../design/Button";
import { PathText } from "../../design/PathText";
import { toast } from "../../design/Toast";
import { commandResult } from "../../host/commandResult";
import type { Project } from "../../protocol/types";
import { CommandFeedback, useUnconfirmedCommand } from "./shared";
import "./work.css";
import { useWarning } from "../../app/warnings";

/** Deletes a one-off: `onDeleted` hears as soon as it leaves the lists, and
 * a toast says when its folder has gone too. */
export function useDeleteOneOff() {
  const { session, profile } = useApp();
  const send = useCommand();
  const [pending, setPending] = useState(false);
  const run = async (project: Project, onDeleted: () => void) => {
    setPending(true);
    const receipt = await send(
      { command: "delete_one_off", project_id: project.project_id },
      { toastErrors: false, label: `Delete ${project.name}` },
    );
    setPending(false);
    if (!receipt || receipt.error) return;
    onDeleted();
    // It is gone from the lists now; the folder follows.
    try {
      await commandResult(session, receipt);
      toast(`Deleted ${project.name}`);
    } catch (e) {
      toast(describeError(e, profile.name), { tone: "stop" });
    }
  };
  return { run, pending };
}

/** Ask before deleting a one-off in full: it leaves Øphio's lists, and its
 * folder, with everything in it, is deleted from the computer. The buttons go
 * to `footer` when the sheet has one. */
export function DeleteOneOff({
  project,
  onCancel,
  onDeleted,
  footer,
}: {
  project: Project;
  onCancel: () => void;
  onDeleted: () => void;
  footer?: HTMLDivElement | null;
}) {
  const { profile } = useApp();
  const remove = useDeleteOneOff();
  const warn = useWarning("project.delete");
  const online = useLink().state === "online";
  const admin = useHost((s) => s.grants?.admin === true);
  const uncertain = useUnconfirmedCommand(
    (c) => c.command === "delete_one_off" && c.project_id === project.project_id,
  );
  const buttons: ReactNode = (
    <div className="work-actions">
      <Button onClick={onCancel}>Keep it</Button>
      <Button
        variant="stop"
        disabled={!online || !admin || uncertain}
        pending={remove.pending}
        onClick={() => { warn.agreed(); void remove.run(project, onDeleted); }}
      >
        Delete {project.name}
      </Button>
    </div>
  );
  return (
    <div className="work-stack">
      <p>
        Delete <strong>{project.name}</strong> from {profile.name}? Its folder
        and everything in it are deleted, and it leaves your lists. This can’t
        be undone.
      </p>
      <p className="fine">
        <PathText path={project.host_path} />
      </p>
      {warn.check}
      {!admin && (
        <p className="fine">
          Only a device with admin access on {profile.name} can do this.
        </p>
      )}
      {footer ? createPortal(buttons, footer) : footer === undefined && buttons}
      <CommandFeedback
        match={(c) =>
          c.command === "delete_one_off" && c.project_id === project.project_id
        }
      />
    </div>
  );
}
