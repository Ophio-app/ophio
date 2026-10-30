// SPDX-License-Identifier: Apache-2.0
import { useCallback, useState, type ReactNode } from "react";
import { useStore } from "../state/store";
import { Button } from "../design/Button";
import { Sheet } from "../design/Sheet";
import { reports, type ReportDraft, type ReportService } from "../diagnostics/reports";
import { MAX_REPORT_WORDS } from "../diagnostics/constants";
import type { ReportExtra, ReportScreenshot } from "../protocol/types";
import { progressWords } from "./words";
import { BundleDescription } from "./BundleDescription";
import { Markup, pictureUrl } from "./Markup";
import "./reports.css";

export function includedWords(draft: ReportDraft): string {
  const b = draft.bundle;
  const parts = [...(b.versions.phone || b.versions.computer ? ["versions"] : []), "where it happened", ...(b.phone_trail.length || b.computer_trail.length ? ["the last few minutes of what the app did"] : []), ...(b.state ? ["screen state"] : []), ...(b.crashes.length ? ["crash details"] : []), ...(b.screenshot ? ["this screen’s picture"] : [])];
  return `Included: ${parts.join(", ")}. Diagnostics leave out code, chats, file and project names.${b.screenshot ? " The picture shows what was on your screen; remove it if you prefer." : ""}${b.extras.length ? " You also chose to share extras shown below." : ""}`;
}
export function ReportSheet({ draft, taskId, onClose, onSent, speech, requiredExtra, service = reports }: { draft: ReportDraft; taskId?: string; onClose(): void; onSent?(): void; speech?: (words: string, onWords: (s: string) => void, onRecording: (v: boolean) => void) => ReactNode; service?: ReportService; requiredExtra?: ReportExtra }) {
  const [, redraw] = useState(0), [removed, setRemoved] = useState<ReportScreenshot | null>(null), [all, setAll] = useState(false), [markup, setMarkup] = useState(false), [sent, setSent] = useState(false), [recording, setRecording] = useState(false), [changing, setChanging] = useState(0), [error, setError] = useState<string | null>(null);
  const refresh = () => redraw(v => v + 1);
  const words = useCallback((s: string) => { service.setWords(draft, s); refresh(); }, [draft, service]);
  const extra = (e: ReportExtra, enabled: boolean) => {
    setChanging(n => n + 1); setError(null);
    const update = service.toggleExtra(draft, e, enabled); refresh();
    void update.catch(() => setError("That extra couldn’t be added. Review what is included below.")).finally(() => { setChanging(n => n - 1); refresh(); });
  };
  const queued = useStore(service, s => s.queued.some(q => q.bundle.report_id === draft.id));
  const picture = draft.bundle.screenshot;
  return <><Sheet open title={sent ? "Got it" : "Report a problem"} onClose={onClose} className="report-sheet" footer={!sent && <Button variant="primary" block disabled={recording || changing > 0} onClick={() => {
    if (requiredExtra && !draft.bundle.extras.some(e => e.type === requiredExtra.type)) { setError("Include the requested extra before sending this follow-up, or close the sheet."); return; }
    try { service.send(draft); setSent(true); onSent?.(); } catch (e) { setError(e instanceof Error ? e.message : "The report couldn’t be saved. Try again."); }
  }}>Send</Button>}>
    {sent ? <div className="report-stack" role="status"><p className="report-receipt">Got it, report <strong>{draft.reference}</strong>. You’ll hear back here.</p>{queued && <p>{progressWords({ state: "queued" })}</p>}<Button block onClick={onClose}>Done</Button></div> : <div className="report-stack">
      {picture && <div><button className="report-picture" type="button" aria-label="Mark up this screen" onClick={() => setMarkup(true)}><img src={pictureUrl(picture)} alt="Ophio’s screen when this report started" /><span>Tap to mark up</span></button><Button size="sm" variant="quiet" onClick={() => { setRemoved(picture); service.setScreenshot(draft); refresh(); }}>Remove</Button></div>}
      {!picture && removed && <Button size="sm" variant="quiet" onClick={() => { service.setScreenshot(draft, removed); setRemoved(null); refresh(); }}>Add the picture back</Button>}
      {speech ? speech(draft.bundle.words ?? "", words, setRecording) : <label className="field"><span className="field__label">What happened?</span><textarea aria-label="What happened?" rows={3} maxLength={MAX_REPORT_WORDS} value={draft.bundle.words ?? ""} onChange={e => words(e.target.value)} placeholder="A sentence is enough. You can leave this blank." /></label>}
      <p className="report-meta">{includedWords(draft)}</p>
      <Button variant="quiet" aria-expanded={all} onClick={() => setAll(v => !v)}>{all ? "Show less" : "See all ›"}</Button>
      {all && <div className="report-stack">{!draft.phoneOnly && <fieldset className="report-extras"><legend>Include more, if you choose</legend>
        {taskId && <label><input type="checkbox" checked={draft.extras.some(e => e.type === "task_transcript")} onChange={e => extra({ type: "task_transcript", task_id: taskId }, e.target.checked)} />This task’s conversation</label>}
        <label><input type="checkbox" checked={draft.extras.some(e => e.type === "names")} onChange={e => extra({ type: "names" }, e.target.checked)} />Real names for the pseudonyms</label>
      </fieldset>}<BundleDescription bundle={draft.bundle} /></div>}
      {error && <p className="error-text" role="status">{error}</p>}
    </div>}
  </Sheet>{markup && picture && <Markup picture={picture} onClose={() => setMarkup(false)} onSave={p => { service.setScreenshot(draft, p); setMarkup(false); refresh(); }} />}</>;
}
