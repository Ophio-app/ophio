// SPDX-License-Identifier: Apache-2.0

import { ChevronRight, CircleCheck, CircleDashed, CircleHelp, CircleMinus, CircleX, Minus, type LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import type { Provenance } from "../protocol/types";
import { Age } from "./Age";

/** `unavailable`: out for now, as input while the computer is locked; nothing failed. */
export type EvidenceStatus = "passed" | "failed" | "pending" | "not_run" | "unknown" | "unavailable" | "info";

const ICON: Record<EvidenceStatus, LucideIcon> = {
  passed: CircleCheck,
  failed: CircleX,
  pending: CircleDashed,
  not_run: Minus,
  unknown: CircleHelp,
  unavailable: CircleMinus,
  info: Minus,
};

const WORD: Record<EvidenceStatus, string> = {
  passed: "Passed",
  failed: "Failed",
  pending: "In progress",
  not_run: "Not run",
  unknown: "Unknown",
  unavailable: "Unavailable",
  info: "",
};

export function provenanceText(p: Provenance, harness?: string): string {
  switch (p) {
    case "native":
      return harness ? `Reported by ${harness}` : "Reported by the agent";
    case "observed":
      return "Observed on the computer";
    case "inferred":
      return "Inferred, not confirmed";
  }
}

/** Evidence row: the claimed result, where the claim came from, its age and how to open it. */
export function EvidenceRow({
  status,
  claim,
  detail,
  source,
  at,
  reason,
  onOpen,
  openLabel,
}: {
  status: EvidenceStatus;
  claim: ReactNode;
  detail?: ReactNode;
  source?: string | null;
  at?: number | null;
  /** The exact reason as the tool gave it: kept out of the row, shown on request. Not for rows that open. */
  reason?: string | null;
  onOpen?: () => void;
  openLabel?: string;
}) {
  const Icon = ICON[status];
  const inner = (
    <>
      <Icon className={`evidence__icon evidence__icon--${status}`} aria-hidden strokeWidth={2.1} />
      <span className="evidence__text">
        <span className="evidence__claim">
          {WORD[status] && <span className="visually-hidden">{WORD[status]}: </span>}
          {claim}
        </span>
        {(detail || source || at != null) && (
          <span className="evidence__meta">
            {detail && <span>{detail}</span>}
            {source && <span>{source}</span>}
            {at != null && <Age at={at} suffix=" ago" />}
          </span>
        )}
        {reason && !onOpen && (
          <details className="evidence__reason">
            <summary>Details</summary>
            <p>{reason}</p>
          </details>
        )}
      </span>
      {status !== "info" && <span className={`evidence__word evidence__word--${status}`}>{WORD[status]}</span>}
      {onOpen && <ChevronRight className="evidence__chevron" aria-hidden />}
    </>
  );
  return onOpen ? (
    <button type="button" className="evidence evidence--action" onClick={onOpen} aria-label={openLabel}>
      {inner}
    </button>
  ) : (
    <div className="evidence">{inner}</div>
  );
}
