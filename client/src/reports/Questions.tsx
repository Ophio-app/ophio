// SPDX-License-Identifier: Apache-2.0
import { useState } from "react";
import { describeError, useApp } from "../app/context";
import { Button } from "../design/Button";
import { reports } from "../diagnostics/reports";
import type { QuestionChoice, ReportQuestion } from "../protocol/types";
import { questionChoices, questionWords, choiceWords } from "./words";
import { openReport } from "./request";
import { computerReports, useReports } from "./store";

export function ReportQuestionCard({ reportId, question, onAnswered }: { reportId: string; question: ReportQuestion; onAnswered?(): void }) {
  const { session, profile } = useApp();
  const [expected, setExpected] = useState<QuestionChoice | null>(null), [error, setError] = useState<string | null>(null);
  const answer = (choice: QuestionChoice) => {
    if (expected) return;
    const perform = () => {
      setExpected(choice); setError(null);
      void reports.answer({ report_id: reportId, question_id: question.question_id, answer: { choice, at: Date.now() } }).then(() => { onAnswered?.(); void computerReports(session).refresh(); }).catch(e => { setExpected(null); setError(describeError(e, profile.name)); void computerReports(session).refresh(); });
    };
    if (choice.choice === "allow" && question.template.template === "may_we_see") {
      const source = reports.get().tasks[reportId];
      const taskId = source?.profileId === profile.profileId ? source.taskId : undefined;
      if (question.template.part === "task_transcript" && !taskId) { setError("This phone no longer has the task link for that conversation. You can choose Don’t allow."); return; }
      openReport({ type: "follow_up", report_id: reportId }, taskId ? { task: taskId } : {}, { extra: question.template.part === "names" ? { type: "names" } : { type: "task_transcript", task_id: taskId! }, onSent: perform });
    } else perform();
  };
  const answered = question.answer?.choice ?? expected;
  return <div className="report-question">
    <p>{questionWords(question.template)}</p>
    {answered ? <p role="status">Answered: {choiceWords(answered)}</p> : <div className="report-question__choices">{questionChoices(question.template).map(c => <Button key={JSON.stringify(c)} onClick={() => answer(c)}>{choiceWords(c)}</Button>)}</div>}
    {error && <p className="error-text" role="status">{error}</p>}
  </div>;
}
export function ReportAttention({ reportId }: { reportId: string }) {
  const { reports: all } = useReports();
  const { ui } = useApp();
  const report = all.find(r => r.report_id === reportId);
  const questions = report?.questions.filter(q => !q.answer) ?? [];
  return <div className="settings-card report-stack"><p>Report {report?.reference ?? "update"}</p>{questions.map(q => <ReportQuestionCard key={q.question_id} reportId={reportId} question={q} />)}<Button variant="quiet" onClick={() => ui.push({ screen: "settings", section: "settings-reports", reportId })}>Open report</Button></div>;
}
export function ReportNeedsYou() {
  const { reports: all } = useReports();
  return <>{all.filter(r => r.questions.some(q => !q.answer)).map(r => <ReportAttention key={r.report_id} reportId={r.report_id} />)}</>;
}
