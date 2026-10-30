// SPDX-License-Identifier: Apache-2.0
import { CircleCheck, CircleHelp, CircleDashed, Square } from "lucide-react";
import type { ReceiptFact, InterventionReceipt } from "../../protocol/types";
import { Age } from "../../design/Age";
import { stopFactWord } from "./stopProgress";
import "./agents.css";

/** An acknowledgement never becomes an all-children-ended claim. */
export function StopFacts({ facts }: { facts: ReceiptFact[] }) {
  return <ul className="agent-facts" aria-label="Stop outcomes">{facts.map((fact, i) => {
    const Icon = fact.status === "done" ? CircleCheck : fact.status === "still_active" ? Square : fact.status === "pending" ? CircleDashed : CircleHelp;
    return <li key={i} className={`agent-fact agent-fact--${fact.status}`}>
      <Icon aria-hidden />
      <div><p><strong>{fact.label.replace(/^Child: /, "")}</strong> · {stopFactWord(fact)}</p>
        {fact.detail && <p className="work-caption">{fact.detail}</p>}</div>
    </li>;
  })}</ul>;
}

export function AgentStopReceipt({ receipt }: { receipt: InterventionReceipt }) {
  return <div className="agent-stop-receipt work-stack">
    <p className="agent-stop-receipt__title">{receipt.summary}</p>
    <p className="work-caption">Stop requested <Age at={receipt.requested_at} suffix=" ago" /></p>
    <StopFacts facts={receipt.facts} />
  </div>;
}
