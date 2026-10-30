// SPDX-License-Identifier: Apache-2.0
import { Check, CircleHelp, Clock, OctagonX, Play, Minus } from "lucide-react";
import { useApp, useHost, shallowEqual } from "../../app/context";
import { useNow } from "../../design/time";
import { Age } from "../../design/Age";
import { Notice } from "../../design/Notice";
import { StateLine } from "../../design/StateLine";
import type { Command, FactStatus } from "../../protocol/types";
import { useStore } from "../../state/store";
import { releaseTarget } from "./useControl";
import { receiptWindow } from "./receiptWindow";
import { heldWithdrawn, heldWithdrawnFact } from "../work/receiptRequests";

export function belongs(
  command: Command,
  targetId?: string,
  taskId?: string,
): boolean {
  if (!["pause_input", "stop_task", "request_human_lease", "take_over", "release_lease", "return_to_agent", "start_private_step", "end_private_step", "withdraw_observation", "lock_desktop"].includes(command.command)) return false;
  return (
    ("target_id" in command && command.target_id === targetId) ||
    ("task_id" in command && command.task_id === taskId) ||
    command.command === "lock_desktop"
  );
}
export function CommandFeedback({
  targetId,
  taskId,
}: {
  targetId?: string;
  taskId?: string;
}) {
  const { session, profile } = useApp();
  const now = useNow();
  const view = useHost((s) => targetId ? s.control[targetId] : undefined);
  const command = useStore(session.commands, (all) =>
    Object.values(all)
      .filter(
        (c) =>
          belongs(c.body, targetId, taskId) ||
          (targetId && c.body.command === "release_lease" &&
            releaseTarget(session, c.body.lease_id) === targetId),
      )
      .at(-1),
  );
  const receipt = useHost((s) =>
    s.interventions
      .filter(
        (r) =>
          (targetId && r.targetId === targetId) ||
          (taskId && r.taskId === taskId) ||
          r.receipt.kind === "lock_desktop",
      )
      .at(-1),
  );
  if (
    !command || now - command.issuedAt > 30_000 ||
    (command.body.command === "end_private_step" && view?.input_state === "agent") ||
    (command.body.command === "start_private_step" && !view?.private_step && !["unknown", "sending"].includes(command.phase)) ||
    (command.body.command === "return_to_agent" && view?.input_state !== "agent" && !["unknown", "sending"].includes(command.phase)) ||
    ((command.body.command === "request_human_lease" || command.body.command === "take_over") && view?.input_state !== "human") ||
    (receipt &&
      receipt.at >= command.issuedAt &&
      !["unknown", "rejected", "sending"].includes(command.phase))
  )
    return null;
  return (
    <StateLine
      phase={
        command.phase === "unknown"
          ? "unknown"
          : command.phase === "sending"
            ? "sending"
            : command.phase === "rejected"
              ? "rejected"
              : command.phase === "not_delivered"
                ? "not_delivered"
                : "host_accepted"
      }
      reportDetails={{ command: command.body.command, error: command.error ?? { code: command.phase }, ...(taskId ? { task: taskId } : {}) }}
      at={command.issuedAt}
      computer={profile.name}
      detail={command.error?.message ?? command.label}
    />
  );
}

const statuses: Record<FactStatus, { icon: typeof Check; label: string }> = {
  done: { icon: Check, label: "Confirmed" },
  pending: { icon: Clock, label: "Pending" },
  unknown: { icon: CircleHelp, label: "Unknown" },
  still_active: { icon: Play, label: "Still active" },
  failed: { icon: OctagonX, label: "Failed" },
  not_applicable: { icon: Minus, label: "Not applicable" },
};
export function InterventionDetails({
  targetId,
  taskId,
}: {
  targetId?: string;
  taskId?: string;
}) {
  const { session } = useApp();
  const current = useStore(receiptWindow(session), (s) => s);
  const now = useNow();
  const view = useHost((s) => targetId ? s.control[targetId] : undefined);
  const records = useHost(
    (s) =>
      s.interventions
        .filter(
          (r) =>
            (targetId && r.targetId === targetId) ||
            (taskId && r.taskId === taskId) ||
            r.receipt.kind === "lock_desktop",
        )
        .slice(-2),
    shallowEqual,
  );
  // As on the task receipt, a Stop's outcomes name the held messages it
  // withdrew from the run it stopped.
  const held = useHost(
    (s) =>
      Object.fromEntries(records.map((r) => {
        if (r.receipt.kind !== "stop_task" || !r.taskId) return [r.seq, 0];
        const items = s.timelines[r.taskId]?.items ?? [];
        const stopped = items.find(
          (i) =>
            i.item.kind === "receipt" &&
            i.item.receipt.kind === "stop_task" &&
            i.item.receipt.requested_at === r.receipt.requested_at,
        );
        return [r.seq, heldWithdrawn(items, stopped?.run_id ?? undefined)];
      })),
    shallowEqual,
  );
  return (
    <>
      {records.filter((r) => current.has(r.seq) && now - r.at < 30_000 && (r.receipt.kind !== "private_step" || !!view?.private_step) && (r.receipt.kind !== "take_over" || view?.input_state === "human") && !(view?.input_state !== "human" && /You have (control|input)/i.test(r.receipt.summary + r.receipt.facts.map((f) => f.label).join(" ")))).map((r) => (
        <section
          key={r.seq}
          className="control-receipt"
          aria-label="Computer receipt"
        >
          <div className="control-row">
            <h2>Computer receipt</h2>
            <Age at={r.at} suffix=" ago" />
          </div>
          <p>{r.receipt.summary}</p>
          <ul>
            {(held[r.seq] ? [...r.receipt.facts, heldWithdrawnFact(held[r.seq]!)] : r.receipt.facts).map((f, i) => {
              const s = statuses[f.status],
                Icon = s.icon;
              return (
                <li
                  key={i}
                  className={`control-fact control-fact--${f.status}`}
                >
                  <Icon size={18} aria-hidden />
                  <div>
                    <strong>{f.label}</strong>
                    <p>
                      {s.label}
                      {f.detail ? ` · ${f.detail}` : ""}
                    </p>
                  </div>
                </li>
              );
            })}
          </ul>
        </section>
      ))}
    </>
  );
}

export function ConnectionNote() {
  const { session } = useApp();
  const link = useStore(session.link, (s) => s);
  const at = useHost((s) => s.lastHeardAt);
  if (link.state === "online") return null;
  return (
    <Notice
      tone={link.state === "failed" ? "stop" : "warn"}
      title={
        link.state === "failed"
          ? "Computer access unavailable"
          : "Computer disconnected"
      }
    >
      {link.state === "failed"
        ? link.detail
        : "Input from this device is off. Computer activity may still be running. Nothing typed here will be sent automatically."}
      {at !== null && (
        <>
          {" "}
          Last heard <Age at={at} suffix=" ago" />.
        </>
      )}
    </Notice>
  );
}
