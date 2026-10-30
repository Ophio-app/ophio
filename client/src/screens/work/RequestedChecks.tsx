// SPDX-License-Identifier: Apache-2.0
import { Section } from "../../design/ListRow";
import type { CheckResult } from "../../protocol/types";

/** Free-form requests are outcomes unless explicitly written or reported as commands. */
export function RequestedChecks({ requests, checks }: { requests: string[]; checks: CheckResult[] }) {
  if (!requests.length) return null;
  return <Section title="Requested completion checks"><div className="work-stack">
    {requests.map((request, i) => {
      const explicit = /^`([^`]+)`$/.exec(request.trim())?.[1];
      const text = explicit ?? request;
      const matches = checks.filter(c => c.provenance !== "inferred" && (c.command === text || c.name === text));
      const command = !!explicit || matches.some(c => c.command === text);
      return <div className="work-card work-stack" key={i}>
        {command ? <code className="work-command">{text}</code> : <p>{text}</p>}
        <p className="work-caption">{command ? "Requested command" : "Requested outcome"}</p>
        {matches.length ? matches.map((c, n) => <p key={n}>{c.status === "not_run" ? "Not run" : c.status.charAt(0).toUpperCase() + c.status.slice(1)} · {c.provenance === "observed" ? "Checked on the computer" : "Reported by the agent"}</p>)
          : <p className="work-muted">{command ? "No result for this command has been reported." : "No result explicitly matching this request has been reported."}</p>}
      </div>;
    })}
  </div></Section>;
}
