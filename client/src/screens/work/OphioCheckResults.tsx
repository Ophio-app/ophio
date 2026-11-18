// SPDX-License-Identifier: Apache-2.0
import { Check, ChevronRight, CircleAlert } from "lucide-react";
import { Notice } from "../../design/Notice";
import "./OphioCheckResults.css";
import type { CheckJob } from "../../protocol/generated/CheckJob";
import type { OphioCheckResult } from "../../protocol/generated/OphioCheckResult";

export function checkOutcomeLabel(result: OphioCheckResult): string {
  switch (result.outcome.kind) {
    case "passed": return "Passed";
    case "failed": return "Failed";
    case "timed_out": return "Time limit reached";
    case "could_not_run": return "Could not run";
    case "skipped_paths": return "Not needed for these changes";
    case "off": return "Off";
    case "stopped": return "Not checked: stopped";
    default: return "Result unknown";
  }
}

export function checksSummary(results: OphioCheckResult[]): string {
  const count = (kind: OphioCheckResult["outcome"]["kind"]) => results.filter(r => r.outcome.kind === kind).length;
  const ran = count("passed") + count("failed") + count("timed_out");
  const parts = [
    [count("passed"), "passed"], [count("failed"), "failed"], [count("timed_out"), "reached the time limit"],
    [count("could_not_run"), "could not run"], [count("skipped_paths"), "not needed for these changes"],
    [count("off"), "off"], [count("stopped"), "stopped"],
  ].filter(([n]) => Number(n) > 0).map(([n, label]) => `${n} ${label}`);
  const lead = ran > 0 ? `Ophio ran ${ran} ${ran === 1 ? "check" : "checks"}` : "Ophio did not finish running any checks";
  return parts.length ? `${lead}: ${parts.join(", ")}.` : "Ophio has not reported any check results yet.";
}

/** These are the computer's results, never commands reported by the agent. */
export function OphioCheckResults({ jobs }: { jobs: CheckJob[] }) {
  if (!jobs.length) return null;
  return <div className="work-stack">
    <h3>Ophio’s results</h3>
    {jobs.map(job => {
      const running = ["pending", "running", "cancelling"].includes(job.state);
      const changed = [...new Set(job.results.flatMap(r => r.inputs_changed))];
      return <div key={job.id} className="work-stack">
        {running && <p role="status">{job.state === "cancelling" ? "Stopping checks…" : "Checking…"} · {job.results.length} of {job.checks.length}</p>}
        <p>{checksSummary(job.results)}</p>
        {job.results.length > 0 && <div className="receipt-checks">
          {job.results.map(result => {
            const warning = ["failed", "timed_out", "could_not_run", "stopped"].includes(result.outcome.kind);
            const Icon = result.outcome.kind === "passed" ? Check : CircleAlert;
            return <details key={result.check_id} className={`receipt-checks__result${warning ? " receipt-checks__result--warning" : ""}`}>
              <summary className="row row--action">
                <span className="row__icon" aria-hidden><Icon /></span>
                <span className="row__main"><span className="row__text">
                  <span className="row__title">{result.name}</span>
                  <span className="row__subtitle">{checkOutcomeLabel(result)}</span>
                </span></span>
                <ChevronRight className="row__chevron" aria-hidden />
              </summary>
              <div className="receipt-checks__output work-check-output">
                <p>{checkOutcomeLabel(result)} · {Math.max(0, Math.round(result.duration_secs))} seconds</p>
                {result.outcome.kind === "could_not_run" && <p>{result.outcome.reason}</p>}
                <pre tabIndex={0}>{result.output || "No output was recorded."}</pre>
              </div>
            </details>;
          })}
        </div>}
        {job.state === "uncertain" && <Notice tone="unknown" report={false} title="Check results are uncertain">Ophio could not confirm that these checks finished or stopped. Do not rely on them as proof that the work passed.</Notice>}
        {job.sandbox?.kind === "unsandboxed" && <Notice tone="warn" report={false}>These checks ran without a sandbox. They had the same file access as a normal program on this computer.</Notice>}
        {job.sandbox?.kind === "contained_without_network_limit" && <Notice tone="warn" report={false}>Ophio could not limit these checks’ internet access.</Notice>}
        {changed.length > 0 && <Notice tone="warn" report={false}>
          This task changed files a check depends on. They differ from the files you approved. You can review and keep the change in the Library.
          <details className="work-details"><summary>Details</summary><ul>{changed.map(path => <li key={path} className="mono">{path}</li>)}</ul></details>
        </Notice>}
        {(job.message || (job.sandbox && "reason" in job.sandbox)) && <details className="work-details">
          <summary>Details</summary>
          {job.message && <p>{job.message}</p>}
          {job.sandbox && "reason" in job.sandbox && <p>{job.sandbox.reason}</p>}
        </details>}
      </div>;
    })}
  </div>;
}
