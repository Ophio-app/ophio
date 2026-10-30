// SPDX-License-Identifier: Apache-2.0
import { CircleX, Clock } from "lucide-react";
import { useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { commandResult } from "../../host/commandResult";
import { PutBack } from "../work/RetryRequest";
import type { ReceiptRequest } from "../work/receiptRequests";
import { useUnconfirmedCommand } from "../work/shared";

/** `withdrawn`: Stop withdrew the message while it was held; it can go back to the composer.
 * `revoked`: it was withdrawn because the chat that sent it lost access. */
export function HeldMessage({ taskId, messageSeq, delivery, withdrawn, revoked, request }: { taskId: string; messageSeq: number; delivery: "queued_for_boundary" | "cancelled"; withdrawn: boolean; revoked: boolean; request: Pick<ReceiptRequest, "item"> }) {
  const { session, profile } = useApp();
  const task = useHost((s) => s.tasks[taskId]);
  const online = useLink().state === "online";
  const canCancel = useHost((s) => s.grants?.tasks === true && s.caughtUp && s.harnesses.find((h) => h.kind === s.tasks[taskId]?.harness)?.capabilities.cancel_queued_message?.state === "available");
  const uncertain = useUnconfirmedCommand((c) => c.command === "cancel_message" && c.task_id === taskId && c.message_seq === messageSeq);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const cancel = async () => {
    setBusy(true); setError(null);
    try {
      const receipt = await session.command({ command: "cancel_message", task_id: taskId, message_seq: messageSeq }, { label: "Cancel held message" });
      await commandResult(session, receipt);
    } catch (e) { setError(describeError(e, profile.name)); }
    finally { setBusy(false); }
  };
  return <div className="talk-held">
    <p className="stateline talk-user__state" role="status">
      {delivery === "cancelled" ? <CircleX aria-hidden /> : <Clock aria-hidden />}
      <span>{delivery === "queued_for_boundary" ? "Held until this turn ends" : withdrawn ? "Withdrawn by Stop · not sent" : revoked ? "Withdrawn when its chat lost access · not sent" : "Cancelled before delivery"}</span>
    </p>
    {delivery === "queued_for_boundary" && canCancel && <Button size="sm" variant="quiet" disabled={!online || uncertain} pending={busy} onClick={() => void cancel()}>Cancel</Button>}
    {delivery === "cancelled" && withdrawn && task && <PutBack task={task} request={request} noteClassName="talk-held__note" size="sm" variant="quiet" />}
    {error && <p className="talk-held__error" role="alert">{error}</p>}
  </div>;
}
