// SPDX-License-Identifier: Apache-2.0
import { useRef, useState } from "react";
import { useApp, useHost, useLink } from "../../app/context";
import { ApprovalCard, ApprovalDetailText, ExactRequest } from "../../design/ApprovalCard";
import { Button } from "../../design/Button";
import { TextArea } from "../../design/Field";
import { focusAfterDecision, OPEN_DECISION } from "../../design/decisionFocus";
import { clockTime, useNow } from "../../design/time";
import { useApprovalDecision } from "../work/shared";
import type { Approval } from "../../protocol/types";

/** A list may embed this beside its open button; opening never decides. */
export function ApprovalQuickActions({ approvalId }: { approvalId: string }) {
  const approval = useHost((s) => s.approvals[approvalId]);
  return approval && ["command", "file_edit", "network", "tool"].includes(approval.kind) ? <QuickDecision key={`${approvalId}/${approval.requested_at}`} approval={approval} /> : null;
}
function QuickDecision({ approval }: { approval: Approval }) {
  const { profile } = useApp();
  const canDecide = useHost((s) => s.grants?.approvals === true);
  const agent = useHost((s) => s.harnesses.find((h) => h.kind === s.tasks[approval.task_id]?.harness)?.display_name ?? "The agent");
  const now = useNow();
  const online = useLink().state === "online";
  const { card } = useApprovalDecision(approval);
  const [denying, setDenying] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [decided, setDecided] = useState(false);
  const quick = useRef<HTMLDivElement>(null);
  if (approval.state !== "pending") return <ApprovalCard approval={approval} agent={agent} myDeviceId={profile.deviceId} canDecide={false} {...card} />;
  const disabled = !online || !canDecide || card.disabled || busy || (approval.expires_at !== null && now >= approval.expires_at);
  const decide = async (allow: boolean) => {
    setBusy(true); setError(null);
    // This row leaves the list once the computer updates it, so without a
    // next request focus goes to where the list begins.
    const refocus = focusAfterDecision(quick.current, quick.current?.closest("section")?.querySelector<HTMLElement>(".section__title"));
    try {
      await card.onDecide(allow ? { decision: "allow" } : { decision: "deny", reason: card.draft.text.trim() || null });
      setDecided(true);
      refocus();
    }
    catch (e) { setError(e instanceof Error ? e.message : "The computer hasn’t confirmed this decision."); }
    finally { setBusy(false); }
  };
  return <div ref={quick} className="talk-quick-approval" {...(canDecide && !decided && { [OPEN_DECISION]: "" })}>
    <p>{agent} · {approval.title}</p>
    {approval.detail && <p className="talk-quick-approval__detail"><ApprovalDetailText approval={approval} /></p>}
    {approval.shortened && <ExactRequest approval={approval} />}
    <p className="talk-quick-approval__detail">Allows: {approval.scope}{approval.expires_at ? ` · expires ${clockTime(approval.expires_at)}` : ""}</p>
    {denying && <TextArea label="Reason (optional)" value={card.draft.text} onChange={card.draft.onChange} rows={2} />}
    <div className="talk-quick-approval__actions">
      <Button variant={denying ? "primary" : "secondary"} disabled={disabled} pending={busy && denying} onClick={() => denying ? void decide(false) : setDenying(true)}>{denying ? "Deny request" : "Deny…"}</Button>
      <Button variant={denying ? "quiet" : "primary"} disabled={disabled} pending={busy && !denying} onClick={() => void decide(true)}>Allow</Button>
    </div>
    {error && <p role="alert">{error}</p>}
  </div>;
}
