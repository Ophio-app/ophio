// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from "react";
import { useApp, useLink } from "../../app/context";
import { Button } from "../../design/Button";
import { ListRow } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import type { HostSession } from "../../host/session";
import type { ContributionPolicy, ContributionPolicyDocument } from "../../protocol/types";
import { roleLabel } from "./codes";

// The control connection executes queries serially, but starts their timeout
// when sent. Keep later reads on the phone until the earlier one has settled.
// Scope this to the session so leaving/reopening Rules cannot bypass the queue.
const reads = new WeakMap<HostSession, Promise<void>>();
function readPolicy(session: HostSession, operationId: string, path: string, live: () => boolean) {
  const result = (reads.get(session) ?? Promise.resolve()).then(() => {
    if (live()) return session.query({ kind: "contribution_policy", operation_id: operationId, path });
    return undefined;
  });
  const settled = result.then(() => undefined, () => undefined);
  reads.set(session, settled);
  void settled.then(() => { if (reads.get(session) === settled) reads.delete(session); });
  return result;
}

export function PolicyReader({ operationId, policy }: { operationId: string; policy: ContributionPolicy }) {
  const { session } = useApp();
  const online = useLink().state === "online";
  const [document, setDocument] = useState<ContributionPolicyDocument | null>(null);
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [failed, setFailed] = useState(false);
  const live = useRef(true);
  useEffect(() => { live.current = true; return () => { live.current = false; }; }, []);
  const read = async () => {
    if (document) { setOpen(!open); return; }
    if (pending || !online) return;
    setPending(true); setFailed(false);
    try {
      const r = await readPolicy(session, operationId, policy.path, () => live.current);
      if (!r) return;
      if (r.document.policy.path !== policy.path || r.document.policy.sha256 !== policy.sha256) throw new Error("Changed rule");
      if (live.current) { setDocument(r.document); setOpen(true); }
    } catch { if (live.current) setFailed(true); }
    finally { if (live.current) setPending(false); }
  };
  return <div className="contribute-policy">
    <ListRow title={<span className="mono">{policy.path}</span>}
      subtitle={`${roleLabel(policy.role)}${document ? ` · ${document.size_bytes.toLocaleString()} bytes` : ""}`}
      trailing={<Button variant="quiet" pending={pending} disabled={!document && !online}
        aria-label={`${open ? "Hide" : "Read"} ${policy.path}`} aria-expanded={open} onClick={() => void read()}>{open ? "Hide" : "Read"}</Button>} />
    {failed && <Notice tone="warn">Couldn’t read and verify this file. Try Read again when the computer is connected.</Notice>}
    {open && document && <pre className="contribute-policy__text" tabIndex={0} aria-label={policy.path}>{document.text}</pre>}
  </div>;
}
