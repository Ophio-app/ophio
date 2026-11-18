// SPDX-License-Identifier: Apache-2.0

import { useEffect, useState } from "react";
import { Check as CheckIcon, CircleAlert, ListChecks, Plus } from "lucide-react";
import { useApp, useHost, useLink } from "../../app/context";
import { useCheckActions, useChecks } from "../../app/checks";
import { CHECKS_FEATURE, CHECKS_UPDATE } from "../../app/checksFeature";
import { Age } from "../../design/Age";
import { Button } from "../../design/Button";
import { TextArea, TextField } from "../../design/Field";
import { Empty, ListRow } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { toast } from "../../design/Toast";
import { commandResult } from "../../host/commandResult";
import type { Check } from "../../protocol/generated/Check";
import type { CheckFacts } from "../../protocol/generated/CheckFacts";
import type { CheckView } from "../../protocol/generated/CheckView";
import type { CheckJob } from "../../protocol/generated/CheckJob";
import type { ChecksDraftView } from "../../protocol/generated/ChecksDraftView";
import type { OphioCheckResult } from "../../protocol/generated/OphioCheckResult";
import type { ProjectChecks } from "../../protocol/generated/ProjectChecks";
import type { ProjectChecksView } from "../../protocol/generated/ProjectChecksView";
import { checkDuration, checkOutcome, proofWords, suggestionTitle } from "./words";
import "./checks.css";

export function Checks({ projectId }: { projectId: string | null }) {
  const c = useChecks(projectId);
  const capabilitiesKnown = useHost((s) => s.capabilities !== null);
  const canWrite = useHost((s) => !!s.grants?.library_write);
  const [opened, setOpened] = useState<{ projectId: string; checkId: string } | null>(null);
  const project = c.projects.find((p) => p.project_id === opened?.projectId);
  const selected = project?.checks.find((v) => v.check.check_id === opened?.checkId);
  if (!capabilitiesKnown) return <p className="fine" role="status">Connecting to read checks…</p>;
  if (!c.supported) return <Notice title="This computer needs updating" report={false}>{CHECKS_UPDATE}</Notice>;
  if (!c.allowed) return <Notice report={false}>This phone is not allowed to read tasks and checks on this computer.</Notice>;
  return <div className="checks-body">
    {!canWrite && <p className="fine">This phone can read checks but is not allowed to change them.</p>}
    {!c.online && <Notice report={false}>The computer isn’t connected. You can read checks already loaded. Reconnect before changing them.</Notice>}
    {c.error && <Notice tone="stop" action={<Button onClick={() => void c.reload()}>Try again</Button>}>{c.error}</Notice>}
    {c.loading && !c.projects.length && <p className="fine" role="status">Reading checks…</p>}
    {!projectId && !c.loading && !c.projects.some((p) => p.checks.length || p.drafts.length) && <Empty icon={ListChecks} title="No checks set up yet">Choose a project in the header to set up its checks.</Empty>}
    {c.projects.filter((p) => projectId || p.checks.length || p.drafts.length).map((p) => <section className="checks-project" key={p.project_id} aria-label={p.project_name}>
      {!projectId && <h2 className="section__title">{p.project_name}</h2>}
      <SuggestedChecksCard projectId={p.project_id} drafts={p.drafts} onChanged={() => void c.reload()} />
      {!p.checks.length && !p.drafts.length ? <EmptyChecks projectId={p.project_id} online={c.online} /> : <>
        <div className="group">{p.checks.map((v) => <div key={v.check.check_id}>
          <ListRow icon={!v.check.enabled ? ListChecks : v.last_result?.outcome.kind === "failed" ? CircleAlert : CheckIcon}
            title={v.check.name} subtitle={<><span className="checks-row__about">{v.check.about.split(/(?<=[.!?])\s/)[0]}</span><span className="checks-row__state">{!v.check.enabled ? "Off" : v.last_result ? <>{checkOutcome(v.last_result)}{v.last_task_title && <> after “{v.last_task_title}”</>}{v.last_finished_at && <> · <Age at={v.last_finished_at} suffix=" ago" /></>}</> : "Not run yet"}</span></>}
            onClick={() => setOpened({ projectId: p.project_id, checkId: v.check.check_id })} chevron />
          {!!v.inputs_changed.length && <ChangedInputs projectId={p.project_id} baseVersion={p.approved.version} view={v} />}
        </div>)}</div>
        <AddCheck projectId={p.project_id} online={c.online} />
      </>}
    </section>)}
    {!projectId && c.withoutChecks > 0 && <p className="fine">{c.withoutChecks} {c.withoutChecks === 1 ? "project has" : "projects have"} no checks yet.</p>}
    {selected && project && <CheckSheet project={project} view={selected} onClose={() => setOpened(null)} />}
  </div>;
}

function EmptyChecks({ projectId, online }: { projectId: string; online: boolean }) {
  const actions = useCheckActions();
  const canWrite = useHost((s) => !!s.grants?.library_write);
  const [sending, setSending] = useState(false);
  return <Empty title="No checks yet"
    action={<Button variant="primary" disabled={!online || !canWrite} pending={sending} onClick={async () => { setSending(true); await actions.ask(projectId, null, ""); setSending(false); }}>Set up checks</Button>}>
    Checks are how Ophio makes sure an agent’s work actually works. After each task, Ophio runs them itself, so you don’t have to take the agent’s word for it.
  </Empty>;
}

function AddCheck({ projectId, online }: { projectId: string; online: boolean }) {
  const [open, setOpen] = useState(false);
  const [request, setRequest] = useState("");
  const [sending, setSending] = useState(false);
  const actions = useCheckActions();
  const canWrite = useHost((s) => !!s.grants?.library_write);
  return <div className="checks-add">
    {open ? <form className="checks-form" onSubmit={async (e) => { e.preventDefault(); setSending(true); if (await actions.ask(projectId, null, request.trim())) { setRequest(""); setOpen(false); } setSending(false); }}>
      <TextArea label="What should Ophio check?" placeholder="that the app still builds for Android" value={request} onChange={setRequest} />
      <Button type="submit" variant="primary" pending={sending} disabled={!request.trim() || !online || !canWrite}>Send</Button>
    </form> : <Button icon={Plus} variant="quiet" onClick={() => setOpen(true)} disabled={!canWrite}>Add a check</Button>}
  </div>;
}

function ChangedInputs({ projectId, baseVersion, view }: { projectId: string; baseVersion: number; view: CheckView }) {
  const actions = useCheckActions();
  const [pending, setPending] = useState(false);
  const enabled = useHost((s) => !!s.grants?.library_write);
  const online = useLink().state === "online";
  return <div className="checks-inputs"><p>Its files changed since you approved it.</p><Button size="sm" variant="quiet" pending={pending} disabled={!enabled || !online} onClick={async () => { setPending(true); await actions.save(projectId, baseVersion, { kind: "keep_inputs", check_id: view.check.check_id }); setPending(false); }}>Keep the change</Button></div>;
}

function Facts({ facts, network }: { facts: CheckFacts; network: boolean }) {
  return <div className="checks-facts"><p>{facts.when}</p>{facts.typical_time && <p>{facts.typical_time}</p>}{network && facts.internet && <p>{facts.internet}</p>}</div>;
}

export function CheckOutput({ result, label = "Show output" }: { result: OphioCheckResult; label?: string }) {
  return <details className="checks-output"><summary>{label}</summary>
    {result.outcome.kind === "could_not_run" && <p>{result.outcome.reason}</p>}
    <pre tabIndex={0}>{result.output || "Nothing was printed."}</pre>
  </details>;
}

function CheckSheet({ project, view, onClose }: { project: ProjectChecksView; view: CheckView; onClose: () => void }) {
  const { session, ui } = useApp();
  const actions = useCheckActions();
  const [request, setRequest] = useState("");
  const [busy, setBusy] = useState(false);
  const [sending, setSending] = useState(false);
  const [editing, setEditing] = useState<{ check: Check; baseVersion: number } | null>(null);
  const [versions, setVersions] = useState<ProjectChecks[]>([]);
  const [job, setJob] = useState<CheckJob | null>(project.unfinished_job ?? null);
  const [starting, setStarting] = useState(false);
  const [confirmingStopped, setConfirmingStopped] = useState(false);
  const [recordingStopped, setRecordingStopped] = useState(false);
  const { online } = useChecks(project.project_id);
  const canWrite = useHost((s) => !!s.grants?.library_write);
  const writable = online && canWrite && !busy;
  useEffect(() => {
    if (!online) return;
    void session.query({ kind: "checks_history", project_id: project.project_id }).then((r) => setVersions(r.versions), actions.fail);
  }, [session, online, project.project_id, project.approved.version]);
  useEffect(() => {
    // The view also identifies standalone jobs, which have no task to look up.
    if (project.unfinished_job) setJob(project.unfinished_job);
  }, [project.unfinished_job]);
  const running = !!job && ["pending", "running", "cancelling"].includes(job.state);
  useEffect(() => {
    if (!online || !running || !job) return;
    let cancelled = false;
    let reported = false;
    let timer: ReturnType<typeof setTimeout>;
    const read = async () => {
      try {
        const r = await session.query({ kind: "check_job", job_id: job.id });
        if (cancelled) return;
        setJob(r.job);
        if (r.job.state === "finished" || r.job.state === "uncertain") void actions.refresh(project.project_id).catch(actions.fail);
      } catch (e) {
        if (!cancelled && !reported) { reported = true; actions.fail(e); }
        if (!cancelled) timer = setTimeout(() => { void read(); }, 2000);
      }
    };
    timer = setTimeout(() => { void read(); }, 750);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [online, running, job, session]);
  const resultJob = job?.purpose.kind === "draft_proof" ? null : job;
  const result = resultJob?.results.find((r) => r.check_id === view.check.check_id) ?? view.last_result;
  const change = async (enabled: boolean) => { setBusy(true); await actions.save(project.project_id, project.approved.version, { kind: "set_enabled", check_id: view.check.check_id, enabled }); setBusy(false); };
  return <><Sheet open onClose={onClose} title={view.check.name} subtitle={project.project_name} size="tall" className="checks-sheet">
    <section><h3>What it checks</h3><p>{view.check.about}</p></section>
    <section><h3>When it runs</h3><Facts facts={view.facts} network={view.check.network} /></section>
    <section><h3>Last result</h3>{result ? <>
      <p className={result.outcome.kind === "failed" ? "checks-failed" : undefined}>{checkOutcome(result)}{!resultJob && view.last_task_title && <> after “{view.last_task_title}”</>}</p>
      <p className="fine">{!resultJob && view.last_finished_at && <><Age at={view.last_finished_at} suffix=" ago" /> · </>}{checkDuration(result.duration_secs)}</p>
      <CheckOutput result={result} />
    </> : <p className="fine">Not run yet.</p>}
    {!!view.inputs_changed.length && <ChangedInputs projectId={project.project_id} baseVersion={project.approved.version} view={view} />}</section>
    <section><h3>Change this check</h3><form className="checks-form" onSubmit={async (e) => { e.preventDefault(); setSending(true); await actions.ask(project.project_id, view.check.check_id, request.trim()); setSending(false); }}>
      <TextArea label="Say what you’d like changed" placeholder="Say what you’d like changed" hideLabel value={request} onChange={setRequest} />
      <Button type="submit" variant="primary" pending={sending} disabled={!writable || !request.trim()}>Send</Button>
    </form></section>
    <section><Button disabled={!writable || running || starting} pending={starting} onClick={async () => { setStarting(true); const next = await actions.run(project.project_id, view.check.check_id); if (next) setJob(next); setStarting(false); }}>Run it now</Button>
      {running && <div className="checks-run" role="status"><p>{job?.state === "cancelling" ? "Stopping check…" : "Checking the project as it is now…"}</p><Button variant="stop-outline" disabled={job?.state === "cancelling" || !online} onClick={async () => {
        if (!job) return; const previous = job; setJob({ ...job, state: "cancelling" });
        try { const r = await commandResult(session, await session.command(job.purpose.kind === "after_run" && job.task_id ? { command: "stop_task", task_id: job.task_id } : { command: "checks_stop_job", job_id: job.id }, { label: "Stop check" })); if (r?.kind === "check_job") setJob(r.job); }
        catch (e) { setJob(previous); actions.fail(e); }
      }}>Stop check</Button></div>}
      {job?.state === "uncertain" && <Notice tone="warn" report={false} action={job.purpose.kind === "after_run" && job.task_id
        ? <Button variant="quiet" onClick={() => ui.openTask(project.project_id, job.task_id!)}>Open task</Button>
        : <Button variant="quiet" disabled={!writable || recordingStopped} onClick={() => setConfirmingStopped(true)}>It has stopped</Button>}>Ophio couldn’t confirm that the check stopped. It may still be running.{job.message && <details className="checks-details"><summary>Details</summary><p>{job.message}</p></details>}</Notice>}
    </section>
    <div className="checks-actions"><Button disabled={!writable} onClick={() => void change(!view.check.enabled)}>{view.check.enabled ? "Turn off" : "Turn on"}</Button>
      <Button variant="quiet" disabled={!writable} onClick={async () => {
        const before = project.approved;
        const version = before.version;
        // Closing follows the instant removal; the old row returns on refusal.
        const saving = actions.save(project.project_id, version, { kind: "delete", check_id: view.check.check_id });
        onClose();
        const saved = await saving;
        if (saved) toast("Check deleted", { action: { label: "Undo", run: () => {
          const current = session.state.get().checksViews?.[project.project_id]?.approved.version;
          if (current !== version + 1) { toast("Checks have changed again. Open their history before undoing.", { tone: "stop" }); return; }
          void actions.save(project.project_id, saved.approved.version, { kind: "undo" }, before);
        } } });
      }}>Delete</Button></div>
    <details className="checks-details"><summary>Details</summary>{editing ? <HandEditor check={editing.check} disabled={!writable} onCancel={() => setEditing(null)} onSave={async (check) => {
      setBusy(true); const saved = await actions.save(project.project_id, editing.baseVersion, { kind: "save", check }); setBusy(false);
      if (!saved) return;
      setEditing(null);
      if (saved.job_id) { try { const r = await session.query({ kind: "check_job", job_id: saved.job_id }); setJob(r.job); } catch (e) { actions.fail(e); } }
    }} /> : <>
      <dl><dt>What it runs</dt><dd><pre>{view.check.run}</pre></dd>{view.check.setup && <><dt>Setup (not run yet)</dt><dd><pre>{view.check.setup}</pre></dd></>}
        <dt>Folders and files it watches</dt><dd>{view.check.paths.length ? <pre>{view.check.paths.join("\n")}</pre> : "Any changed file"}</dd>
        <dt>Time limit</dt><dd>{view.facts.time_limit.replace(/^Time limit:\s*/i, "")}</dd><dt>Internet</dt><dd>{view.check.network ? "Allowed" : "Not allowed"}</dd>
        <dt>Files it depends on</dt><dd>{view.check.inputs.length ? <pre>{view.check.inputs.join("\n")}</pre> : "None listed"}</dd>
      </dl><Button variant="quiet" disabled={!writable} onClick={() => setEditing({ check: view.check, baseVersion: project.approved.version })}>Edit by hand</Button>
    </>}</details>
    {project.approved.version > 0 && <footer className="checks-history"><p className="fine">Changed <Age at={project.approved.changed_at} suffix=" ago" /> by you.</p>
      <Button variant="quiet" disabled={!writable || versions[0]?.version !== project.approved.version || (project.approved.version !== 1 && versions.length < 2)} onClick={async () => { setBusy(true); await actions.save(project.project_id, project.approved.version, { kind: "undo" }, versions[1] ?? { ...project.approved, checks: [], input_fingerprints: [] }); setBusy(false); }}>Undo this change</Button>
    </footer>}
  </Sheet>
  {confirmingStopped && job?.state === "uncertain" && <Sheet open onClose={() => setConfirmingStopped(false)} title="Has it stopped?" footer={<div className="checks-actions">
    <Button variant="primary" pending={recordingStopped} disabled={!writable || recordingStopped} onClick={async () => {
      setRecordingStopped(true);
      try {
        const r = await commandResult(session, await session.command({ command: "checks_stop_job", job_id: job.id, confirmed_stopped: true }, { label: "Record check as stopped" }));
        if (r?.kind !== "check_job") throw new Error("The computer did not confirm this change. The check may still be running.");
        setJob(r.job);
        setConfirmingStopped(false);
        void actions.refresh(project.project_id).catch(actions.fail);
      } catch (e) { setConfirmingStopped(false); actions.fail(e); }
      finally { setRecordingStopped(false); }
    }}>Record as stopped</Button>
    <Button variant="quiet" disabled={recordingStopped} onClick={() => setConfirmingStopped(false)}>Cancel</Button>
  </div>}>
    <p>Only confirm if you’re sure it has stopped. Ophio will record the check as not checked and free the project for other work.</p>
    <p>If Ophio can still see the check running, it will refuse and keep the project reserved.</p>
  </Sheet>}
  </>;
}

function HandEditor({ check, disabled, onSave, onCancel }: { check: Check; disabled: boolean; onSave: (check: Check) => Promise<void>; onCancel: () => void }) {
  const [draft, setDraft] = useState(check);
  const [paths, setPaths] = useState(check.paths.join("\n"));
  const [inputs, setInputs] = useState(check.inputs.join("\n"));
  const [saving, setSaving] = useState(false);
  const field = <K extends keyof Check>(key: K, value: Check[K]) => setDraft((d) => ({ ...d, [key]: value }));
  const list = (text: string) => text.split("\n").map((v) => v.trim()).filter(Boolean);
  return <form className="checks-form checks-hand" onSubmit={async (e) => { e.preventDefault(); setSaving(true); await onSave({ ...draft, paths: list(paths), inputs: list(inputs) }); setSaving(false); }}>
    <TextField label="Name" value={draft.name} onChange={(v) => field("name", v)} required />
    <TextArea label="What it checks" value={draft.about} onChange={(v) => field("about", v)} required />
    <TextArea label="What it runs" value={draft.run} onChange={(v) => field("run", v)} required />
    <TextArea label="Setup (stored, not run yet)" value={draft.setup ?? ""} onChange={(v) => field("setup", v || null)} />
    <TextArea label="Folders and files it watches" hint="One pattern per line. Leave empty to watch any file." value={paths} onChange={setPaths} />
    <label className="field"><span className="field__label">When it runs</span><select className="field__input" value={draft.when} onChange={(e) => field("when", e.target.value as Check["when"])}><option value="every_task">After a task changes files</option><option value="before_merge">Before merging (not run yet)</option></select></label>
    <TextField label="Time limit in seconds" type="number" min={10} max={3600} value={String(draft.timeout_secs)} onChange={(v) => field("timeout_secs", Number(v))} required />
    <label className="checks-checkbox"><input type="checkbox" checked={draft.network} onChange={(e) => field("network", e.target.checked)} />Uses the internet</label>
    <TextArea label="Files it depends on" hint="One project-relative file per line." value={inputs} onChange={setInputs} />
    <p className="fine">Saving approves these details and runs the check once. It stays saved even if it fails.</p>
    <div className="checks-actions"><Button type="submit" variant="primary" disabled={disabled} pending={saving}>Save and run</Button><Button variant="quiet" onClick={onCancel}>Cancel</Button></div>
  </form>;
}

/** Used unchanged in the Library and in a task's receipt. */
export function SuggestedChecksCard({ projectId, drafts, onChanged }: { projectId: string; drafts: ChecksDraftView[]; onChanged?: () => void }) {
  const [selected, setSelected] = useState<ChecksDraftView | null>(null);
  const supported = useHost((s) => !!s.capabilities?.features.includes(CHECKS_FEATURE));
  const current = drafts.find((d) => d.draft.id === selected?.draft.id) ?? selected;
  const open = drafts.filter((d) => ["ready", "proving", "stale"].includes(d.draft.state));
  if (!supported || !open.length && !selected) return null;
  return <><div className="checks-suggestions">{open.map((d) => <ListRow key={d.draft.id} icon={ListChecks} title="Suggested checks" subtitle={suggestionTitle(d)} onClick={() => setSelected(d)} chevron />)}</div>
    {current && <DraftReview key={current.draft.id} projectId={projectId} view={current} onClose={() => setSelected(null)} onChanged={onChanged} />}
  </>;
}

function DraftReview({ projectId, view, onClose, onChanged }: { projectId: string; view: ChecksDraftView; onClose: () => void; onChanged?: () => void }) {
  const c = useChecks(projectId);
  const actions = useCheckActions();
  const canWrite = useHost((s) => !!s.grants?.library_write);
  const [keep, setKeep] = useState(() => view.draft.items.map((_, i) => i));
  const [busy, setBusy] = useState(false);
  const [optimisticDone, setOptimisticDone] = useState<"apply" | "discard" | null>(null);
  const project = c.projects.find((p) => p.project_id === projectId);
  const stale = view.draft.state === "stale" || !!project && project.approved.version !== view.draft.base_version;
  const proving = view.draft.state === "proving";
  const allowed = c.online && canWrite && !busy;
  const act = async (selected: number[] | null) => {
    setBusy(true); setOptimisticDone(selected ? "apply" : "discard");
    if (await actions.draftAction(projectId, view, selected)) { onChanged?.(); onClose(); }
    else { setOptimisticDone(null); setBusy(false); }
  };
  return <Sheet open onClose={onClose} title="Suggested checks" subtitle={project?.project_name} size="tall" className="checks-sheet" footer={<div className="checks-actions">
    {stale ? <Button variant="primary" pending={busy} disabled={!allowed} onClick={async () => { setBusy(true); if (await actions.ask(projectId, null, view.draft.request?.trim() ? view.draft.request : "Suggest checks for this project again using the latest approved checks.")) onClose(); setBusy(false); }}>Ask again</Button>
      : <Button variant="primary" pending={busy} disabled={!allowed || proving || !keep.length} onClick={() => void act(keep)}>Use these</Button>}
    <Button variant="quiet" disabled={!allowed} onClick={() => void act(null)}>Discard</Button>
  </div>}>
    {optimisticDone ? <p role="status">{optimisticDone === "apply" ? "Updating checks…" : "Discarding suggestion…"}</p> : <>
      {stale && <Notice tone="warn" report={false} title="These checks changed since this suggestion was made">Ask again to get a suggestion for the latest checks. Nothing from this draft will be applied.</Notice>}
      {proving && <p className="fine" role="status">Ophio is trying these checks on the project as it is now. You can use them once that finishes.</p>}
      <p className="fine">Leave out anything you don’t want. Only the checks you keep will be approved.</p>
      {view.draft.items.map((item, i) => {
        const check = item.kind === "remove" ? null : item.check;
        const name = check?.name ?? project?.checks.find((v) => item.kind === "remove" && v.check.check_id === item.check_id)?.check.name ?? "Check to remove";
        const proof = check && view.draft.proofs.find((p) => p.check_id === check.check_id);
        return <section className={`checks-draft-item${keep.includes(i) ? "" : " checks-draft-item--left-out"}`} key={i}>
          <div className="checks-draft-heading"><h3>{name}</h3><Button size="sm" variant="quiet" aria-pressed={!keep.includes(i)} onClick={() => setKeep((k) => k.includes(i) ? k.filter((n) => n !== i) : [...k, i].sort((a, b) => a - b))}>{keep.includes(i) ? "Leave out" : "Keep this"}</Button></div>
          <p className="checks-item-kind">{item.kind === "add" ? "New check" : item.kind === "change" ? "Change" : "Remove"}{!keep.includes(i) ? " · Left out" : ""}</p>
          {item.kind === "remove" ? <p>{item.reason}</p> : <>
            <p>{item.kind === "change" ? item.note : item.check.about}</p>
            {item.kind === "change" && !!view.differences[i]?.length && <div className="checks-differences"><p className="fine">What Ophio found changed</p><ul>{view.differences[i]!.map((line, n) => <li key={n}>{line}</li>)}</ul></div>}
            {view.facts[i] && <Facts facts={view.facts[i]!} network={item.check.network} />}
            {proof ? <div className="checks-proof"><p>{proofWords(proof)}</p><CheckOutput result={proof} /></div> : <p className="fine">{proving ? "Waiting for Ophio’s result…" : "Ophio has not run this check yet."}</p>}
          </>}
        </section>;
      })}
    </>}
  </Sheet>;
}
