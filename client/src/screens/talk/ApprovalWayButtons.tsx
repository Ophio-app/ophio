// SPDX-License-Identifier: Apache-2.0
//
// The request card's buttons, from the ways forward the computer says it
// takes (`approvalWays`): Allow or Not now for an agent's request for more
// access in a chat; Switch to Work for one in a Plan or Ask task; "Switch to
// Work and allow" or "Turn into a task" where Allow can't work. They plug
// into the card's `actions` slot, so they share its pending state, its
// refusals and where focus goes once decided. A computer older than
// switch_to_work_v1 keeps the card's own Decline and Allow.

import { shallowEqual, useApp, useHost } from "../../app/context";
import { AllowButton, DeclineButton, type ApprovalActions } from "../../design/ApprovalCard";
import { Button } from "../../design/Button";
import type { Approval } from "../../protocol/types";
import { approvalWays, switchesToWork } from "./approvalWays";
import { otherHolder, switchToWork } from "./switchToWork";


/**
 * The card's buttons for `approval`, or undefined to keep its own Decline
 * and Allow (an older computer, or a question or code, which answer in the
 * card itself).
 */
export function useApprovalWayButtons(approval: Approval): ApprovalActions | undefined {
  const { session } = useApp();
  const task = useHost((s) => s.tasks[approval.task_id] ?? null);
  const switches = useHost((s) => switchesToWork(s.capabilities));
  const agent = useHost((s) => s.harnesses.find((h) => h.kind === s.tasks[approval.task_id]?.harness)?.display_name ?? "The agent");
  const project = useHost((s) => s.projects[approval.project_id]?.name ?? "the project");
  const holder = useHost((s) => otherHolder(s, approval.project_id, approval.task_id), shallowEqual);
  const changesTasks = useHost((s) => s.grants?.tasks === true);
  if (!switches || !approval.actions || approval.kind === "question" || approval.kind === "login_code") return undefined;
  const { ways, note } = approvalWays(approval, { task, agent, project, holder, switches, changesTasks });
  // As the card's own pair: the way out first, the main choice last.
  const shown = [...ways.filter((w) => w.action === "deny"), ...ways.filter((w) => w.action !== "deny")];
  return (ctx) => <>
    {shown.map((way) => {
      switch (way.action) {
        case "deny": return <DeclineButton key="deny" ctx={ctx} label={`${way.label}…`} />;
        case "allow": return <AllowButton key="allow" ctx={ctx} label={way.label} />;
        case "switch_to_work":
        case "turn_into_task":
          return <Button key="switch" variant="primary" pending={ctx.sending === "switch"} disabled={ctx.disabled || !task}
            onClick={() => task && void ctx.run("switch", () => switchToWork(session, task, approval.approval_id))}>
            {ctx.sending === "switch" ? "Switching" : way.label}
          </Button>;
        case "answer": return null;
      }
    })}
    {note && <p className="approval__way-note">{note}</p>}
  </>;
}
