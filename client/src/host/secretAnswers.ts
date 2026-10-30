// SPDX-License-Identifier: Apache-2.0
import type { Approval, ApprovalDecision, QueryData } from "../protocol/types";

export function retainedDecision(decision: ApprovalDecision, approval?: Approval): ApprovalDecision {
  return decision.decision === "answer" && (!approval || approval.questions?.some(q => q.secret))
    ? { decision: "answer", text: "", answers: [] } : decision;
}

/** Older computers may replay retained plaintext. Never keep it on the phone. */
export function retainedApproval(approval: Approval): Approval {
  if (!approval.delivery) return approval;
  const decision = retainedDecision(approval.delivery.decision, approval);
  return decision === approval.delivery.decision ? approval
    : { ...approval, delivery: { ...approval.delivery, decision } };
}

export function retainedQuery(data: QueryData): QueryData {
  switch (data.kind) {
    case "approval": return { ...data, approval: retainedApproval(data.approval) };
    case "snapshot":
    case "run": return { ...data, approvals: data.approvals.map(retainedApproval) };
    case "task":
    case "task_history": return { ...data, records: { ...data.records, approvals: data.records.approvals.map(retainedApproval) } };
    default: return data;
  }
}
