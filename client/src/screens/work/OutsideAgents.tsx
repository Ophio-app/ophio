// SPDX-License-Identifier: Apache-2.0
import { PRODUCT_NAME } from "../../brand";
import { useEffect, useRef, useState } from "react";
import { Eye, Square } from "lucide-react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { Age } from "../../design/Age";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { Empty, Section } from "../../design/ListRow";
import { CapabilityNote, Notice } from "../../design/Notice";
import { useBackLayer } from "../../design/back";
import { useNow } from "../../design/time";
import type { HostSession } from "../../host/session";
import type { ObservedSession, ObservedSessions } from "../../protocol/types";
import { useRoute } from "../../shell/Screen";
import { AgentStopFeedback, useAgentStop } from "./AgentChildren";
import { harnessLabel } from "./logic";
import "./agents.css";

const cache = new WeakMap<HostSession, ObservedSessions>();
export function OutsideAgents() {
  const { session, profile } = useApp();
  const link = useLink();
  const { active } = useRoute();
  const granted = useHost(s => s.grants?.tasks === true);
  const [view, setView] = useState(() => cache.get(session));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [retry, setRetry] = useState(0);
  const [reviewing, setReviewing] = useState(false);
  useEffect(() => {
    if (!active || link.state !== "online" || !granted || reviewing) return;
    let alive = true;
    let pending = false;
    const refresh = async () => {
      if (pending || document.visibilityState === "hidden") return;
      pending = true;
      setBusy(true);
      try {
        const result = await session.query({ kind: "observed_sessions" });
        if (alive) { cache.set(session, result.view); setView(result.view); setError(null); }
      } catch (e) { if (alive) setError(describeError(e, profile.name)); }
      finally { pending = false; if (alive) setBusy(false); }
    };
    void refresh();
    const timer = setInterval(() => void refresh(), 10_000);
    document.addEventListener("visibilitychange", refresh);
    return () => { alive = false; clearInterval(timer); document.removeEventListener("visibilitychange", refresh); };
  }, [active, link.state, granted, session, profile.name, retry, reviewing]);
  const processes = view?.sessions.filter(s => s.process_id !== null) ?? [];
  const records = view?.sessions.filter(s => s.process_id === null) ?? [];
  return <Section title="Outside the app">
    <div className="agent-outside">
      <p className="work-muted">Agent processes observed on {profile.name}. Opening this list gives the app no control over them.</p>
      {view?.checked_at != null && <p className="work-caption">Last checked <Age at={view.checked_at} suffix=" ago" /></p>}
      {(error || view?.error) && <Notice tone="unknown" title="Observation unavailable">{error ?? view?.error}</Notice>}
      {!granted && <Notice>Task access is needed to check outside sessions.</Notice>}
      {!view && granted && <p className="work-muted">{link.state === "online" ? "Checking outside sessions…" : "Connect to check outside sessions."}</p>}
      {processes.map(row => <OutsideRow key={row.observed_id} row={row} observationFailed={!!error || !!view?.error} refresh={() => setRetry(n => n + 1)} onReview={setReviewing} />)}
      {view && !error && !view.error && !processes.length && <Empty icon={Eye} title="No outside agents observed">A scan can only report processes the computer can identify.</Empty>}
      {!!records.length && <details className="agent-children"><summary>Saved session metadata · {records.length}</summary>
        <p className="work-caption">A saved record does not establish whether an agent is running.</p>
        {records.map(row => <OutsideRow key={row.observed_id} row={row} refresh={() => setRetry(n => n + 1)} onReview={setReviewing} />)}
      </details>}
      <p className="work-caption">{view?.coverage.join(" ") ?? "Claude Code and Codex are checked. Hermes, OpenClaw and Pi are not checked."}</p>
      <Button disabled={link.state !== "online" || !granted || busy || reviewing} onClick={() => setRetry(n => n + 1)}>Refresh outside sessions</Button>
    </div>
  </Section>;
}

export function OutsideRow({ row, refresh, onReview, observationFailed = false }: { row: ObservedSession; refresh: () => void; onReview?: (reviewing: boolean) => void; observationFailed?: boolean }) {
  const { profile } = useApp();
  const link = useLink();
  const now = useNow();
  const project = useHost(s => row.project_id ? s.projects[row.project_id] : undefined);
  const grants = useHost(s => s.grants);
  const key = `outside:${row.observed_id}`;
  const { store, intent, pending } = useAgentStop(key);
  const [review, setReview] = useState<ObservedSession | null>(null);
  const reviewHeading = useRef<HTMLHeadingElement>(null);
  useEffect(() => { onReview?.(review !== null); return () => { if (review) onReview?.(false); }; }, [onReview, review]);
  useBackLayer(review !== null, () => setReview(null));
  useEffect(() => { if (review) { reviewHeading.current?.focus(); reviewHeading.current?.scrollIntoView?.({ block: "start", behavior: "instant" }); } }, [review]);
  const ended = intent?.receipt?.facts.every(f => f.status === "done") === true;
  const needsTerminal = intent && !pending && !ended;
  const stale = observationFailed || now - row.observed_at > 30_000;
  const canStop = link.state === "online" && !!grants?.tasks && !!grants.terminal && !pending && !stale;
  return <article className="work-card work-stack" data-observed-id={row.observed_id}>
    <h3 className="work-card-title">{harnessLabel(row.harness)}{row.process_id !== null ? ` · ${row.process_id}` : " session record"}</h3>
    <p>{ended ? "Process group ended" : row.running ? link.state === "online" && !stale ? "Running outside the app" : "Last observed running outside the app" : row.process_id !== null ? "Process no longer running" : "Running state not known"}</p>
    <p className="work-caption">{project ? `Project: ${project.name}` : "No project match"} · Observed <Age at={row.observed_at} suffix=" ago" /></p>
    <dl className="agent-outside__metadata">
      <div><dt>Working directory</dt><dd className="mono">{row.cwd ?? "Not available"}</dd></div>
      {row.executable && <div><dt>Executable</dt><dd className="mono">{row.executable}</dd></div>}
      {row.native_session && <div><dt>Session</dt><dd className="mono">{row.native_session}</dd></div>}
      {row.started_at !== null && <div><dt>Started</dt><dd><Age at={row.started_at} suffix=" ago" /></dd></div>}
      {row.last_activity !== null && <div><dt>Last activity</dt><dd><Age at={row.last_activity} suffix=" ago" /></dd></div>}
    </dl>
    {row.running && !ended && <>
      {needsTerminal ? <p role="status">Stop is not confirmed. Stop this agent in its terminal on {profile.name}. Force stop is unavailable because {PRODUCT_NAME} did not start its process.</p> : row.stop.state === "available" ? <>
        <Button variant="stop-outline" size="lg" icon={Square} disabled={!canStop} onClick={() => setReview(row)}>Review Stop · {harnessLabel(row.harness)} {row.process_id}</Button>
        {!grants?.terminal && <p className="work-caption">Task and terminal access are both required to stop an outside process.</p>}
        {stale && <p className="work-caption">Refresh this observation before stopping the process.</p>}
      </> : <CapabilityNote what="Stop" availability={row.stop} />}
    </>}
    <AgentStopFeedback stopKey={key} />
    {review && <section className="agent-outside-confirm work-stack" aria-label="Stop this outside agent?">
      <h4 ref={reviewHeading} tabIndex={-1}>Stop this outside agent?</h4>
      <p>The app did not start this agent. Stop sends a termination signal to its process group, including tools still running in that group.</p>
      <p>Unsaved work may be lost. File changes already made remain. Detached processes may continue.</p>
      <dl className="agent-outside__metadata"><div><dt>Executable</dt><dd className="mono">{review.executable}</dd></div><div><dt>Working directory</dt><dd className="mono">{review.cwd ?? "Not available"}</dd></div></dl>
      {review.revision !== row.revision && <Notice tone="unknown">The observation changed while this review was open. Close it and review the current process again.</Notice>}
      <div className="agent-outside-confirm__actions">
      <Button variant="secondary" onClick={() => setReview(null)}>Keep running</Button>
      <Button variant="stop" size="lg" icon={Square} disabled={!canStop || !review || review.revision !== row.revision}
        onClick={() => {
          if (!review) return;
          const request = review;
          setReview(null);
          announce(`Stop requested for outside ${harnessLabel(request.harness)} process ${request.process_id}`);
          void store.request(key, { command: "stop_observed_session", observed_id: request.observed_id, confirmed: true }, request.revision, `Stop outside ${harnessLabel(request.harness)} ${request.process_id}`).then(refresh);
        }}>Stop process group {review.process_id}</Button>
      </div>
    </section>}

  </article>;
}
