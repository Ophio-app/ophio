// SPDX-License-Identifier: Apache-2.0
//
// The one vocabulary for where a draft, message or command is. Each phase is
// a distinct fact; none is rounded up to a stronger one.

import { Check, CheckCheck, CircleHelp, CircleX, Clock, HardDrive, Smartphone, type LucideIcon } from "lucide-react";
import type { ReportContextInput } from "../diagnostics/bundle";
import { openReport } from "../reports/request";
import { Button } from "./Button";
import { Age } from "./Age";

export type DeliveryPhase =
  | "saved_on_phone"
  | "sending"
  | "host_accepted"
  | "agent_unconfirmed"
  | "waiting_in_next_up"
  | "queued_for_boundary"
  | "native_delivered"
  | "unknown"
  | "not_delivered"
  | "arrived_late"
  | "rejected"
  | "failed"
  | "saved_on_computer";

const SPEC: Record<DeliveryPhase, { icon: LucideIcon | null; tone: string; label: (who: string) => string }> = {
  saved_on_phone: { icon: Smartphone, tone: "neutral", label: () => "Saved on this device" },
  sending: { icon: null, tone: "neutral", label: () => "Sending" },
  host_accepted: { icon: Check, tone: "neutral", label: (who) => `Sent to ${who}` },
  agent_unconfirmed: { icon: Check, tone: "neutral", label: (who) => `Handed to ${who} · not yet confirmed by the agent` },
  waiting_in_next_up: { icon: Clock, tone: "neutral", label: () => "Waiting in Next up" },
  queued_for_boundary: { icon: Clock, tone: "neutral", label: () => "Waiting for the next turn" },
  native_delivered: { icon: CheckCheck, tone: "ok", label: () => "Delivered to the agent" },
  unknown: { icon: CircleHelp, tone: "warn", label: () => "Outcome unknown" },
  not_delivered: { icon: CircleX, tone: "neutral", label: () => "Not delivered" },
  arrived_late: { icon: CircleX, tone: "neutral", label: () => "Arrived after the run ended; not applied" },
  rejected: { icon: CircleX, tone: "stop", label: () => "Refused" },
  failed: { icon: CircleX, tone: "stop", label: () => "Failed" },
  saved_on_computer: { icon: HardDrive, tone: "ok", label: (who) => `Saved on ${who}` },
};

export function StateLine({
  phase,
  at,
  computer = "computer",
  detail,
  className,
  reportDetails,
}: {
  phase: DeliveryPhase;
  at?: number | null;
  computer?: string;
  detail?: string | null;
  className?: string;
  reportDetails?: Partial<ReportContextInput>;
}) {
  const s = SPEC[phase];
  const Icon = s.icon;
  return (
    <p className={["stateline", `stateline--${s.tone}`, className].filter(Boolean).join(" ")} role="status">
      {Icon ? <Icon aria-hidden strokeWidth={2.2} /> : <span className="stateline__spinner" aria-hidden />}
      <span>{s.label(computer)}</span>
      {["rejected", "failed", "not_delivered"].includes(phase) && <Button size="sm" variant="quiet" onClick={() => openReport({ type: "error" }, { error: { code: phase, message: detail ?? undefined }, ...reportDetails })}>Report</Button>}
      {detail && <span className="stateline__detail">· {detail}</span>}
      {at != null && phase !== "sending" && (
        <>
          <span aria-hidden>·</span>
          <Age at={at} suffix=" ago" />
        </>
      )}
    </p>
  );
}

/**
 * A safety choice on its way: the control beside it still shows what is in
 * force (docs/interaction-contract.md). `to` says what it is changing to.
 */
export function Changing({ to, className }: { to: string; className?: string }) {
  return (
    <p className={["stateline", "stateline--neutral", className].filter(Boolean).join(" ")} role="status">
      <span className="stateline__spinner" aria-hidden />
      <span>Changing…</span>
      <span className="stateline__detail">· {to}</span>
    </p>
  );
}
