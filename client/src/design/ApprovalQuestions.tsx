// SPDX-License-Identifier: Apache-2.0
//
// An agent's multiple-choice questions, answered on the approval card: each
// option with what it means, one or several chosen, or the person's own words.

import { Check } from "lucide-react";
import { useId } from "react";
import type { ApprovalQuestion, QuestionAnswer } from "../protocol/types";
import { TextArea, TextField } from "./Field";

/** One question's answer in progress. */
export interface Picked {
  labels: string[];
  own: string;
}

export const unpicked = (questions: ApprovalQuestion[]): Picked[] => questions.map(() => ({ labels: [], own: "" }));

/** Chosen labels in the order offered, then the person's own words. */
export function choices(question: ApprovalQuestion, picked: Picked): string[] {
  const labels = question.options.map(o => o.label).filter(label => picked.labels.includes(label));
  const own = picked.own.trim();
  return own && !labels.includes(own) ? [...labels, own] : labels;
}

/** Every question answered, with the words that read the answers back. */
export function answered(questions: ApprovalQuestion[], picked: Picked[]): { text: string; answers: QuestionAnswer[] } | null {
  const answers = questions.map((q, i) => ({ ...(q.id != null ? { id: q.id } : {}), question: q.question, choices: choices(q, picked[i]!) }));
  if (answers.some(a => !a.choices.length)) return null;
  const text = answers.length === 1 ? answers[0]!.choices.join(", ")
    : answers.map((a, i) => `${questions[i]!.header ?? a.question}: ${a.choices.join(", ")}`).join("; ");
  return { text, answers };
}

export function ApprovalQuestions({ questions, picked, onChange, disabled, titled = true }: {
  questions: ApprovalQuestion[];
  picked: Picked[];
  onChange: (picked: Picked[]) => void;
  disabled: boolean;
  /** A single question is already the title above them. */
  titled?: boolean;
}) {
  const id = useId();
  const set = (i: number, next: Picked) => onChange(picked.map((p, j) => (j === i ? next : p)));
  return questions.map((q, i) => {
    const p = picked[i]!;
    const multi = q.multi_select ?? false;
    // One choice only: an option, or the person's own words instead.
    const pick = (label: string) =>
      set(i, multi ? { ...p, labels: p.labels.includes(label) ? p.labels.filter(l => l !== label) : [...p.labels, label] } : { labels: [label], own: "" });
    return (
      <fieldset key={q.id ?? q.question} className="approval__question" disabled={disabled}>
        <legend className="approval__ask">
          {q.header && <span className="approval__ask-header">{q.header}</span>}
          {/* A single question is already the card's title. */}
          <span className={questions.length > 1 || !titled ? "approval__ask-text" : "visually-hidden"}>{q.question}</span>
          {multi && <span className="approval__ask-hint">Choose one or more</span>}
        </legend>
        {q.options.length > 0 && <div className="approval__options">
          {q.options.map((o, k) => {
            const on = p.labels.includes(o.label);
            return (
              <label key={o.label} className={["approval__option", multi && "approval__option--multi", on && "is-on"].filter(Boolean).join(" ")}>
                <input
                  type={multi ? "checkbox" : "radio"}
                  className="approval__option-input"
                  name={`${id}-${i}`}
                  checked={on}
                  onChange={() => pick(o.label)}
                  aria-labelledby={`${id}-${i}-${k}`}
                  aria-describedby={o.description ? `${id}-${i}-${k}-does` : undefined}
                />
                <span className="approval__option-text">
                  <span className="approval__option-label" id={`${id}-${i}-${k}`}>{o.label}</span>
                  {o.description && <span className="approval__option-does" id={`${id}-${i}-${k}-does`}>{o.description}</span>}
                </span>
                <span className="approval__option-mark" aria-hidden>{multi && on && <Check strokeWidth={3} />}</span>
              </label>
            );
          })}
        </div>}
        {/* A secret, like a password, is typed hidden and kept off the screen. */}
        {q.secret ? (
          <TextField
            type="password"
            autoComplete="off"
            label={q.options.length ? "Or type it" : "Type it"}
            value={p.own}
            onChange={own => set(i, { labels: multi || !own.trim() ? p.labels : [], own })}
          />
        ) : (
          <TextArea
            label={!q.options.length ? "Your answer" : multi ? "Add your own answer" : "Or answer in your own words"}
            value={p.own}
            onChange={own => set(i, { labels: multi || !own.trim() ? p.labels : [], own })}
            rows={1}
            maxRows={4}
          />
        )}
      </fieldset>
    );
  });
}

/** What the person answered, one line per question. */
export function AnswerLines({ questions, answers, text }: { questions: ApprovalQuestion[]; answers: QuestionAnswer[]; text: string }) {
  if (!answers.length) return <p className="approval__settled-what">{questions.some(q => q.secret) ? "Answered" : `Your answer: ${text}`}</p>;
  return answers.map(a => {
    const asked = questions.find(q => (a.id == null || q.id === a.id) && q.question === a.question);
    // A secret answer isn't read back.
    const said = asked?.secret ? (answers.length > 1 ? `${asked.header ?? a.question}: answered` : "Answered")
      : `${answers.length > 1 ? `${asked?.header ?? a.question}: ` : "Your answer: "}${a.choices.join(", ")}`;
    return <p key={a.id ?? a.question} className="approval__settled-what">{said}</p>;
  });
}
