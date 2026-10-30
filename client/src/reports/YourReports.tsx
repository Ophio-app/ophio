// SPDX-License-Identifier: Apache-2.0
import { useEffect, useRef, useState } from "react";
import { useCommandControl } from "../app/commandControl";
import { describeError, useApp, useHost } from "../app/context";
import { Button } from "../design/Button";
import { ListRow } from "../design/ListRow";
import { Notice } from "../design/Notice";
import { Sheet } from "../design/Sheet";
import { reports } from "../diagnostics/reports";
import type { ReportDetail, ReportSettings, ReportSummary } from "../protocol/types";
import { AppUpdate, useAppOffer } from "../screens/settings/AppUpdate";
import { BundleDescription } from "./BundleDescription";
import { ReportQuestionCard } from "./Questions";
import { useReports } from "./store";
import { fixedBuild, intakeWords, latestProgress, progressWords, reportAbout } from "./words";
import "./reports.css";

export function YourReports({ reportId }: { reportId?: string }) {
  const { ui, session, profile } = useApp();
  const { view, reports: all, remote, local, error } = useReports();
  const savedRevision = useRef(0);
  const control = useCommandControl<Pick<ReportSettings, "sending" | "ring">>({
    kind: "instant", value: view?.settings ?? local.settings,
    equal: (a, b) => a.sending === b.sending && a.ring === b.ring,
    describe: e => describeError(e, profile.name),
    send: async next => {
      const revision = Math.max(remote.get().view?.settings.revision ?? local.settings.revision, savedRevision.current);
      if (remote.supported()) await session.command({ command: "set_report_settings", ...next }, { label: "Report settings", expectedRevision: revision });
      savedRevision.current = revision + 1;
      reports.setSettings({ ...next, revision: revision + 1 });
      await remote.refresh();
    },
  });
  const settings = control.value;
  const change = (next: Pick<ReportSettings, "sending" | "ring">) => { void control.set(next, { label: "Report settings", what: "report settings" }); };
  const selected = all.find(r => r.report_id === reportId);
  return <div className="report-stack">
    {reportId ? selected ? <ReportDetails report={selected} /> : <p role="status">This report isn’t available here yet.</p> : <>
      <div className="settings-card report-stack">
        <label className="settings-check"><input type="checkbox" checked={settings.sending} onChange={e => change({ ...settings, sending: e.target.checked })} />Send reports</label>
        <label className="settings-check"><input type="checkbox" checked={settings.ring !== "everyone"} onChange={e => change({ ...settings, ring: e.target.checked ? "early" : "everyone" })} />Hear about fixes early</label>
        <p className="settings-meta">Hearing about a fix doesn’t install an update.</p>
        {view && intakeWords(view.intake) && <p className="settings-meta">{intakeWords(view.intake)}</p>}
      </div>
      {view?.halted && <Notice report={false} title="This build was paused">Build {view.halted.build} was paused. {view.halted.back_to ? `On the computer, run ophio update --rollback to go back to build ${view.halted.back_to}.` : "Check for an update on the computer with ophio update."}</Notice>}
      {all.length ? <div className="group">{all.map(r => <ListRow key={r.report_id} title={`Report ${r.reference}`} subtitle={<>{reportAbout(r)}<span className="work-row-line">{progressWords(latestProgress(r))}</span></>} chevron onClick={() => ui.push({ screen: "settings", section: "settings-reports", reportId: r.report_id })} />)}</div> : <p>Your reports will appear here.</p>}
    </>}
    {!!error && <Notice tone="warn" report={false}>{describeError(error, profile.name)}</Notice>}
  </div>;
}
function ReportDetails({ report }: { report: ReportSummary }) {
  const { session, profile } = useApp();
  const { remote, local } = useReports();
  const offered = useAppOffer();
  const build = useHost(s => s.host?.companion_version);
  const [detail, setDetail] = useState<ReportDetail | null>(null), [see, setSee] = useState(false), [confirm, setConfirm] = useState(false), [withdrawn, setWithdrawn] = useState(false), [problem, setProblem] = useState<string | null>(null);
  const status = latestProgress(report), fixed = fixedBuild(status);
  const bundle = local.queued.find(q => q.bundle.report_id === report.report_id)?.bundle ?? local.sent[report.report_id] ?? detail?.bundle;
  useEffect(() => {
    if (!remote.supported() || bundle || status.state === "withdrawn") return;
    let alive = true;
    void session.query({ kind: "report", report_id: report.report_id }).then(r => { if (alive) setDetail(r.detail ?? null); }).catch(e => { if (alive) setProblem(describeError(e, profile.name)); });
    return () => { alive = false; };
  }, [session, report.report_id, remote, bundle, status.state, profile.name]);
  const gone = withdrawn || status.state === "withdrawn";
  return <div className="report-stack">
    <h2>Report {report.reference}</h2><p>{reportAbout(report)}</p>
    {gone ? <p role="status">{progressWords({ state: "withdrawn" })}</p> : <>
      <p>{progressWords(status)}</p>
      {fixed && fixed !== build && <div className="settings-card report-stack">{offered?.offer?.version === fixed && <><p>Fixed in this update</p><AppUpdate found={offered} /></>}<p className="settings-meta">If the phone’s update isn’t offered yet, update Ophio on the computer with ophio update, then check Versions here.</p></div>}
      {report.questions.filter(q => !q.answer).map(q => <ReportQuestionCard key={q.question_id} reportId={report.report_id} question={q} />)}
      <Button variant="quiet" aria-expanded={see} onClick={() => setSee(v => !v)}>See what was sent</Button>
      {see && (bundle ? <BundleDescription bundle={bundle} /> : <p>The sent bundle isn’t available on this phone.</p>)}
      <Button variant="quiet" onClick={() => setConfirm(true)}>Withdraw</Button>
    </>}
    <details><summary>Status history</summary><ol>{report.history.map((h, i) => <li key={`${h.at}-${i}`}><p>{progressWords(h.status)}</p><time dateTime={new Date(h.at).toISOString()}>{new Date(h.at).toLocaleString()}</time></li>)}</ol></details>
    {problem && <Notice tone="warn" report={false}>{problem}</Notice>}
    <Sheet open={confirm} title={`Withdraw report ${report.reference}?`} onClose={() => setConfirm(false)} footer={<Button variant="primary" onClick={() => {
      setConfirm(false);
      void reports.withdraw(report.report_id).then(() => { setWithdrawn(true); void remote.refresh(); }).catch(e => setProblem(describeError(e, profile.name)));
    }}>Withdraw report</Button>}><p>Any copy sent to the project is deleted with its attachments. Its reference stays here.</p></Sheet>
  </div>;
}
