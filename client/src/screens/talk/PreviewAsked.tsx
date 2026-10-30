// SPDX-License-Identifier: Apache-2.0
//
// What an agent asked about a preview it showed. The person answers beside
// the page as they try it, then, back in Ophio, puts the answers in Next up
// for the task's next turn or leaves them here, in Needs you, for later.

import { useState } from "react";
import { describeError, useApp, useHost, useLink } from "../../app/context";
import { ApprovalQuestions, choices } from "../../design/ApprovalQuestions";
import { announce } from "../../design/Announcer";
import { Button } from "../../design/Button";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import { clockTime } from "../../design/time";
import { commandResult } from "../../host/commandResult";
import type { ApprovalQuestion } from "../../protocol/types";
import type { PreviewQuestions } from "../control/Previews";
import { answerMessage, answerRoute, anyAnswered, readAnswers, writeAnswers, type PreviewAnswers } from "./previewAnswers";

export interface Asked {
  questions: ApprovalQuestion[];
  answers: PreviewAnswers;
  save(update: (answers: PreviewAnswers) => PreviewAnswers): void;
  reviewing: boolean;
  setReviewing(open: boolean): void;
  /** What the preview carries for answering beside the page. */
  ask: PreviewQuestions | undefined;
}

export function usePreviewAsked(taskId: string, at: number, questions: ApprovalQuestion[]): Asked {
  const [answers, setAnswers] = useState(() => readAnswers(taskId, at, questions));
  const [reviewing, setReviewing] = useState(false);
  // Every change is kept on this device at once, so a closed app loses nothing.
  const save = (update: (answers: PreviewAnswers) => PreviewAnswers) => setAnswers(prev => {
    const next = update(prev);
    writeAnswers(taskId, at, next);
    return next;
  });
  const ask = questions.length && answers.sentAt == null ? {
    questions,
    picked: answers.picked,
    onPicked: (picked: typeof answers.picked) => save(a => ({ ...a, picked })),
    onSubmit: (picked: typeof answers.picked) => {
      save(a => ({ ...a, picked, submitted: true }));
      setReviewing(true);
    },
  } : undefined;
  return { questions, answers, save, reviewing, setReviewing, ask };
}

/** What the person answered, one line per question they answered. */
function AnswerLines({ questions, picked }: { questions: ApprovalQuestion[]; picked: PreviewAnswers["picked"] }) {
  return questions.map((q, i) => {
    const said = picked[i] ? choices(q, picked[i]) : [];
    if (!said.length) return null;
    const what = questions.length > 1 ? `${q.header ?? q.question}: ` : "Your answer: ";
    return <p key={q.question} className="approval__settled-what">{what + said.join(", ")}</p>;
  });
}

export function PreviewAsked({ asked, taskId, label }: { asked: Asked; taskId: string; label: string }) {
  const { session, profile } = useApp();
  const online = useLink().state === "online";
  const { questions, answers, save, reviewing, setReviewing } = asked;
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const route = useHost(s => answerRoute(s.tasks[taskId]));
  const ready = anyAnswered(questions, answers.picked);
  const asks = questions.length === 1 ? "The agent asks a question about this page" : `The agent asks ${questions.length} questions about this page`;
  const sendLabel = route === "next_turn" ? "Send after this turn" : "Put in Next up";

  const send = async () => {
    const text = answerMessage(label, questions, answers.picked);
    const task = session.state.get().tasks[taskId];
    const how = answerRoute(task);
    if (!text) return;
    if (!task || !how) {
      setError("This task can’t take a message now. Your answers are kept on this device.");
      return;
    }
    setSending(true);
    setError(null);
    try {
      await commandResult(session, await session.command(
        how === "next_up"
          ? { command: "queue_continuation", task_id: task.task_id, text, authorization: "auto_start" }
          : { command: "send_message", task_id: task.task_id, text, bundle: null, delivery: "next_turn" },
        how === "next_up"
          ? { label: "Put your answers in Next up", expectedRevision: task.revision }
          : { label: "Send your answers after this turn" },
      ));
      save(a => ({ ...a, submitted: true, sentAt: Date.now() }));
      setReviewing(false);
      announce(how === "next_up" ? "Your answers are in Next up." : "Your answers go to the agent after this turn.");
    } catch (e) {
      setError(describeError(e, profile.name));
    } finally {
      setSending(false);
    }
  };
  const leave = () => {
    if (ready) save(a => ({ ...a, submitted: true }));
    setReviewing(false);
    if (ready) announce("Your answers are kept on this device until you send them.");
  };

  return (
    <div className="talk-shown__asked">
      {answers.sentAt != null ? (
        <>
          <p className="fine">{`You sent your answers · ${clockTime(answers.sentAt)}`}</p>
          <AnswerLines questions={questions} picked={answers.picked} />
        </>
      ) : answers.submitted && ready ? (
        <Notice
          title="Your answers aren’t sent yet"
          action={
            <div className="preview-actions">
              <Button variant="primary" size="sm" pending={sending} disabled={!online || sending || !route} onClick={() => void send()}>{sendLabel}</Button>
              <Button size="sm" disabled={sending} onClick={() => setReviewing(true)}>Edit answers</Button>
            </div>
          }
        >
          <AnswerLines questions={questions} picked={answers.picked} />
          {error && !reviewing && <p className="fine">{error}</p>}
        </Notice>
      ) : (
        <p className="fine talk-shown__asks">
          {`${asks}. Answer beside it when you open it, or here.`}{" "}
          <Button size="sm" onClick={() => setReviewing(true)}>{ready ? "Keep answering" : "Answer here"}</Button>
        </p>
      )}
      <Sheet
        open={reviewing}
        onClose={leave}
        title="Your answers"
        subtitle={label}
        footer={
          <div className="preview-capture-actions">
            {error && <Notice tone="stop">{error}</Notice>}
            {!route && <p className="fine">This task can’t take a message now. Your answers are kept on this device.</p>}
            <p className="fine">
              {route === "next_turn"
                ? "The agent is still working. It reads your answers after this turn."
                : "Put in Next up starts the task again with your answers when the project is free. Leave them here to send later; the task stays in Needs you."}
            </p>
            <div className="preview-actions">
              <Button variant="primary" pending={sending} disabled={!ready || !online || sending || !route} onClick={() => void send()}>{sendLabel}</Button>
              <Button disabled={sending} onClick={leave}>Leave in Needs you</Button>
            </div>
          </div>
        }
      >
        <ApprovalQuestions
          questions={questions}
          picked={answers.picked}
          onChange={picked => save(a => ({ ...a, picked }))}
          disabled={sending}
          titled={false}
        />
      </Sheet>
    </div>
  );
}
