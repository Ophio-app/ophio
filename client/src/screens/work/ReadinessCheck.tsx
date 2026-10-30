// SPDX-License-Identifier: Apache-2.0
import { RefreshCw } from "lucide-react";
import type { ReactNode } from "react";
import { useApp, useCommand, useHost, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { StateLine } from "../../design/StateLine";
import { useStore } from "../../state/store";
import "./ReadinessCheck.css";

/** Admission is not a completed check. Keep progress in the session so a
 * navigation or a lost receipt cannot turn a pending check into success. */
export function ReadinessCheck({ label = "Check again", children }: { label?: string; children?: ReactNode }) {
  const { session } = useApp();
  const run = useCommand();
  const online = useLink().state === "online";
  const allowed = useHost(s => s.grants?.admin === true);
  const last = useStore(session.commands, commands => Object.values(commands)
    .filter(c => c.body.command === "run_readiness_check").at(-1));
  const progress = useHost(s => last ? s.commandStages[last.id] : undefined);
  const pending = last?.phase === "sending" ||
    ((last?.phase === "accepted" || last?.phase === "duplicate") &&
      (!progress || progress.stage === "host_accepted" || progress.stage === "native_acknowledged"));
  const unknown = last?.phase === "unknown" || progress?.stage === "uncertain" || progress?.stage === "reconciliation_required";
  const failed = last?.phase === "rejected" || progress?.stage === "failed";
  return (
    <div className="readiness-check">
      <div className="readiness-check__actions">
        <Button size="sm" icon={RefreshCw} pending={pending} disabled={!online || !allowed}
          onClick={() => void run({ command: "run_readiness_check" }, { label })}>
          {pending ? "Checking…" : label}
        </Button>
        {children}
      </div>
      {!allowed && <p className="fine">Device management permission is needed to run checks.</p>}
      {!online && <p className="fine">Reconnect to check the computer.</p>}
      {unknown ? <StateLine phase="unknown" detail="The computer hasn’t confirmed these checks." />
        : failed ? <StateLine phase="failed" detail={progress?.error?.message ?? last?.error?.message ?? "The computer couldn’t run these checks."} />
        : progress?.stage === "observed" ? <p className="fine" role="status">Checks updated.</p>
        : last?.phase === "not_delivered" ? <StateLine phase="not_delivered" />
        : pending ? <p className="fine" role="status">Waiting for the computer’s results.</p> : null}
    </div>
  );
}
