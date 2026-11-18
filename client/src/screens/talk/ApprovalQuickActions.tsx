// SPDX-License-Identifier: Apache-2.0
import { useApp, useHost } from "../../app/context";
import { ApprovalCard } from "../../design/ApprovalCard";
import { useApprovalDecision } from "../work/shared";
import type { Approval } from "../../protocol/types";

/**
 * A request answered where it is listed: the same card as everywhere else,
 * without a frame of its own, under the row that names its task. Opening the
 * row never decides.
 */
export function ApprovalQuickActions({ approvalId }: { approvalId: string }) {
  const approval = useHost((s) => s.approvals[approvalId]);
  return approval && (["command", "file_edit", "network", "tool"].includes(approval.kind) || !!approval.access_request) ? <QuickDecision key={`${approvalId}/${approval.requested_at}`} approval={approval} /> : null;
}
function QuickDecision({ approval }: { approval: Approval }) {
  const { profile } = useApp();
  const canDecide = useHost((s) => s.grants?.approvals === true);
  const agent = useHost((s) => s.harnesses.find((h) => h.kind === (s.runs[approval.run_id]?.harness ?? s.tasks[approval.task_id]?.harness))?.display_name ?? "The agent");
  const { card } = useApprovalDecision(approval);
  return <div className="talk-quick-approval">
    <ApprovalCard approval={approval} agent={agent} myDeviceId={profile.deviceId} canDecide={canDecide && approval.state === "pending"} embedded {...card}
      // This row leaves the list once the computer updates it, so without a
      // next request focus goes to where the list begins.
      focusFallback={() => document.activeElement?.closest("section")?.querySelector<HTMLElement>(".section__title")} />
  </div>;
}
