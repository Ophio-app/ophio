// SPDX-License-Identifier: Apache-2.0
import { useEffect, useState } from "react";
import { Bug, X } from "lucide-react";
import { useApp } from "../app/context";
import { toast } from "../design/Toast";
import { notificationSettings } from "../platform/notificationSettings";
import { latestProgress, progressWords } from "./words";
import { Button } from "../design/Button";
import { announce } from "../design/Announcer";
import { Sheet } from "../design/Sheet";
import { addCrashSource, crashes } from "../diagnostics/crashes";
import { describeBundle, enforceSize } from "../diagnostics/bundle";
import { reports, type ReportDraft } from "../diagnostics/reports";
import { screenOn, routePattern } from "../diagnostics/routes";
import { takeNativeCrashes, onScreenshot, screenshotSupported, captureOwnView } from "../platform/reportNative";
import type { CrashRecord } from "../protocol/types";
import { useStore } from "../state/store";
import { ReportSheet } from "./ReportSheet";
import { reportContext, reportRequest, openReport } from "./request";
import { ReportSpeech } from "./ReportSpeech";
import { useReports } from "./store";
import { CHIP_MS, ScreenshotOffer, dismissCrashes, undismissedCrashes } from "./prompts";
import { RouteProvider } from "../shell/Screen";

let nativeTaken = false;
addCrashSource(async () => {
  if (nativeTaken) return [];
  nativeTaken = true;
  const { crashes: found, updatedAt } = await takeNativeCrashes();
  if (updatedAt) crashes.forgetBefore(updatedAt);
  return found;
});

export function ConnectedReports() {
  const { ui, session, platform, profile } = useApp();
  const { view } = useReports();
  useEffect(() => reportContext(() => {
    const state = ui.get(), route = screenOn(state);
    const taskId = "taskId" in route ? route.taskId : route.screen === "talk" && state.projectId && !state.taskList ? state.taskByProject[state.projectId] : undefined;
    const task = taskId ? session.state.get().tasks[taskId] : undefined;
    return { screen: routePattern(route), ...(task ? { task: task.task_id, harness: task.harness, model: task.model ?? undefined } : {}) };
  }), [ui, session]);
  useEffect(() => {
    const settings = notificationSettings(session), stop = settings.follow();
    const known = new Map(reports.get().direct.map(r => [r.report_id, JSON.stringify(latestProgress(r))]));
    const off = reports.subscribe(() => {
      for (const report of reports.get().direct) {
        const status = latestProgress(report), key = JSON.stringify(status), previous = known.get(report.report_id);
        known.set(report.report_id, key);
        if (!previous || key === previous || settings.current().off.includes("reports") || !["question", "reproduced", "fixed", "shipped"].includes(status.state)) continue;
        const route = screenOn(ui.get());
        if (route.screen === "settings" && route.reportId === report.report_id) continue;
        const message = `Report ${report.reference} · ${progressWords(status)}`;
        if (document.visibilityState !== "hidden") toast(message, { action: { label: "Open report", run: () => ui.push({ screen: "settings", section: "settings-reports", reportId: report.report_id }) } });
        else void Promise.resolve().then(() => platform.notify?.({ id: `${profile.profileId}/report:${report.report_id}`, title: `Report ${report.reference}`, body: progressWords(status), category: status.state === "question" ? "needs_you" : "finished", route: { kind: "report", profileId: profile.profileId, reportId: report.report_id } })).catch(() => {});
      }
    });
    return () => { stop(); off(); };
  }, [session, ui, platform, profile.profileId]);
  const current = screenOn(ui.get());
  return <RouteProvider value={{ tab: ui.get().tab, route: current, depth: 0, active: true }}><ReportsOverlay computerCrashes={view?.crashes} connected /></RouteProvider>;
}
const NONE: CrashRecord[] = [];
export function ReportsOverlay({ computerCrashes = NONE, connected = false }: { computerCrashes?: CrashRecord[]; connected?: boolean }) {
  const request = useStore(reportRequest, s => s);
  const [draft, setDraft] = useState<ReportDraft | null>(null), [phoneCrashes, setPhoneCrashes] = useState<CrashRecord[]>([]), [, render] = useState(0);
  const [chip, setChip] = useState(false), [offer] = useState(() => new ScreenshotOffer());
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { let active = true; void crashes.collect().then(c => { if (active) setPhoneCrashes(c); }); return () => { active = false; }; }, []);
  useEffect(() => {
    if (!request) { setDraft(null); return; }
    let active = true;
    // Capture waits for a frame without the chip, and starts before a modal
    // covers the screen. Nothing is captured later.
    const picture = painted().then(captureOwnView);
    void Promise.all([reports.startReport(request.origin, request.context), picture]).then(async ([next, shot]) => {
      if (!active) return;
      if (shot) reports.setScreenshot(next, { ...shot, marked: false });
      if (request.crashes) {
        const keys = new Map([...next.bundle.crashes, ...request.crashes].map(c => [JSON.stringify(c), c]));
        next.bundle.crashes = [...keys.values()].sort((a, b) => b.at - a.at);
        if (next.bundle.crashes[0]) next.bundle.signature = { ...next.bundle.signature, code: next.bundle.crashes[0].code, build: next.bundle.crashes[0].build };
        next.bundle = enforceSize(next.bundle);
        next.description = describeBundle(next.bundle);
      }
      if (request.extra) {
        await reports.toggleExtra(next, request.extra, true);
        if (!next.bundle.extras.some(e => e.type === request.extra!.type)) throw new Error("The requested extra isn’t available. Connect to the original computer to review it.");
      }
      if (active) { setDraft(next); setChip(false); }
    }).catch((e: unknown) => { if (active) { setError(e instanceof Error ? e.message : "The report couldn’t be prepared. Try again."); reportRequest.set(null); } });
    return () => { active = false; };
  }, [request]);
  useEffect(() => {
    let stopped = false, off = () => {};
    void screenshotSupported().then(supported => {
      if (!supported || stopped) return;
      off = onScreenshot(() => { if (!reportRequest.get() && offer.allowed() && document.visibilityState !== "hidden") { setChip(true); announce("Report a problem?"); } });
    });
    return () => { stopped = true; off(); };
  }, [offer]);
  useEffect(() => {
    if (!chip) return;
    const timer = setTimeout(() => setChip(false), CHIP_MS);
    return () => clearTimeout(timer);
  }, [chip, offer]);
  const pending = undismissedCrashes([...phoneCrashes, ...computerCrashes]);
  // A script or render error leaves Ophio running; only an exit or panic stops it.
  const closed = pending.some(c => c.kind === "system" || c.kind === "panic");
  const clearCrashes = () => { dismissCrashes(pending); crashes.acknowledge(pending.filter(c => c.side === "phone")); setPhoneCrashes([]); render(n => n + 1); };
  return <>
    {draft && request && <ReportSheet key={draft.id} draft={draft} taskId={request.context.task} requiredExtra={request.extra} onClose={() => reportRequest.set(null)} onSent={() => { if (request.origin.type === "crash") clearCrashes(); request.onSent?.(); }} speech={connected ? (words, onWords, onRecording) => <ReportSpeech words={words} onWords={onWords} onRecording={onRecording} /> : undefined} />}
    <Sheet open={pending.length > 0 && !request} title={closed ? "Ophio closed unexpectedly" : "Ophio ran into a problem"} onClose={clearCrashes} footer={<><Button variant="primary" onClick={() => openReport({ type: "crash" }, {}, { crashes: pending })}>Send</Button><Button variant="quiet" onClick={clearCrashes}>Not now</Button></>}><p>Send what happened?</p></Sheet>
    {chip && !request && <aside className="report-chip" aria-label="Screenshot report"><div className="toast">
      <Bug className="toast__icon" aria-hidden />
      <p className="toast__text">Something wrong on this screen?</p>
      <button type="button" className="toast__action" onClick={() => { offer.accept(); setChip(false); openReport({ type: "screenshot" }); }}>Report</button>
      <button type="button" className="toast__close" aria-label="Dismiss report offer" title="Dismiss" onClick={() => { offer.dismiss(); setChip(false); }}><X /></button>
    </div></aside>}
    {error && <Sheet open title="Report a problem" onClose={() => setError(null)}><p role="status">{error}</p><Button onClick={() => { setError(null); openReport(); }}>Try again</Button></Sheet>}
  </>;
}
/** Resolves once the frame after the current one is on screen. */
function painted(): Promise<void> {
  if (typeof requestAnimationFrame !== "function") return Promise.resolve();
  return new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
}
