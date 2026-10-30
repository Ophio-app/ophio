// SPDX-License-Identifier: Apache-2.0

import { CircleAlert, CircleHelp, Info, OctagonX, TriangleAlert, type LucideIcon } from "lucide-react";
import type { ReportContextInput } from "../diagnostics/bundle";
import { openReport } from "../reports/request";
import { Button } from "./Button";
import { recordNotice } from "../diagnostics/instrumentation";
import { useEffect, type ReactNode } from "react";
import type { Availability } from "../protocol/types";
import { remedyText } from "./remedy";

export type NoticeTone = "info" | "warn" | "stop" | "unknown";

const ICONS: Record<NoticeTone, LucideIcon> = { info: Info, warn: TriangleAlert, stop: OctagonX, unknown: CircleHelp };

/** An inline explanation. Used for capability gaps, stale data and failures. */
export function Notice({ tone = "info", code = "inline_notice", title, children, action, icon, className, report = true, reportDetails }: { tone?: NoticeTone; code?: string; title?: ReactNode; children?: ReactNode; action?: ReactNode; icon?: LucideIcon; className?: string; report?: boolean | "always"; reportDetails?: Partial<ReportContextInput> }) {
  useEffect(() => recordNotice(code, tone === "info" ? "info" : "error"), [code, tone]);
  const Icon = icon ?? ICONS[tone];
  return (
    <div className={["notice", `notice--${tone}`, className].filter(Boolean).join(" ")} role={tone === "stop" ? "alert" : undefined}>
      <Icon className="notice__icon" aria-hidden strokeWidth={2.1} />
      <div className="notice__text">
        {title && <p className="notice__title">{title}</p>}
        {children && <div className="notice__body">{children}</div>}
        {(action || (report && (tone !== "info" || report === "always"))) && <div className="notice__action">{action}{report && (tone !== "info" || report === "always") && <Button size="sm" variant="quiet" onClick={() => openReport({ type: "error" }, { error: { code, ...(typeof children === "string" ? { message: children } : {}) }, ...reportDetails })}>Report</Button>}</div>}
      </div>
    </div>
  );
}

/** Human wording for an availability value. */
export function availabilityText(a: Availability): { ok: boolean; label: string; detail: string | null } {
  switch (a.state) {
    case "available":
      return { ok: true, label: "Available", detail: null };
    case "unsupported":
      return { ok: false, label: "Not supported", detail: a.reason };
    case "permission_missing":
      return { ok: false, label: "Needs setup", detail: remedyText(a.remedy) };
    case "temporarily_unavailable":
      return { ok: false, label: "Unavailable right now", detail: a.reason };
    case "revoked":
      return { ok: false, label: "Access revoked", detail: "Pair this device again to restore it." };
    case "incompatible":
      return { ok: false, label: "Incompatible", detail: a.reason };
    case "unknown":
      return { ok: false, label: "Not known", detail: a.reason };
  }
}

/**
 * Capability explanation: what works, what is missing and the exact route to
 * fix it — instead of a silently disabled control.
 */
export function CapabilityNote({ what, availability, action }: { what: string; availability: Availability; action?: ReactNode }) {
  if (availability.state === "available") return null;
  const t = availabilityText(availability);
  const tone: NoticeTone = availability.state === "unknown" ? "unknown" : availability.state === "revoked" ? "stop" : "warn";
  return (
    <Notice tone={tone} icon={availability.state === "permission_missing" ? CircleAlert : undefined} title={`${what}: ${t.label.toLowerCase()}`} action={action}>
      {t.detail}
    </Notice>
  );
}
