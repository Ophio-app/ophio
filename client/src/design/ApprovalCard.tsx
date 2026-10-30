// SPDX-License-Identifier: Apache-2.0
//
// Approval card: the exact effect, who asks, the grant scope, the revision it
// is bound to and its expiry. A decision quotes that revision; if the request
// changed or was answered elsewhere, the card says so instead of acting.

import { CircleCheck, CircleHelp, CircleX, KeyRound, MessageCircleQuestion, SendHorizontal, ShieldAlert } from "lucide-react";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import type { Approval, ApprovalDecision, ApprovalDetailLine, ApprovalKind, ApprovalLineChange, Principal } from "../protocol/types";
import { Age } from "./Age";
import { AnswerLines, ApprovalQuestions, answered, unpicked } from "./ApprovalQuestions";
import { Button } from "./Button";
import { focusAfterDecision, OPEN_DECISION } from "./decisionFocus";
import { TextArea, TextField } from "./Field";
import { clockTime, useNow } from "./time";
import { InlineCode } from "./InlineCode";

const ASK: Record<ApprovalKind, string> = {
  command: "run a command",
  file_edit: "change files",
  network: "use the network",
  tool: "use a tool",
  question: "ask you",
  computer_use: "control the desktop",
  login_use: "sign in with one of your logins",
  login_code: "get a sign-in code from you",
  github_post: "post on GitHub as you",
  other: "do something that needs your approval",
};

export interface DecisionFailure {
  code: string;
  message: string;
}

export interface ApprovalCardProps {
  approval: Approval;
  /** e.g. "Claude Code · Opus 5.5" */
  agent: string;
  myDeviceId: string | null;
  canDecide: boolean;
  /** Keep a task-bound answer/reason in the caller’s persisted draft. */
  draft?: { text: string; onChange: (text: string) => void };
  /** Temporarily block decisions without changing the phone’s grant. */
  disabled?: boolean;
  disabledReason?: string;
  /** Resolves when the computer accepted the decision; rejects with a DecisionFailure-like error. */
  onDecide: (d: ApprovalDecision) => Promise<void>;
  /** Sends the sign-in code a `login_code` request asks for, the same way. */
  onCode?: (code: string) => Promise<void>;
  onOpenTask?: () => void;
}

function byWhom(p: Principal | null, me: string | null): string {
  if (!p) return "";
  switch (p.kind) {
    case "device":
      return p.device_id === me ? "by you" : "on another device";
    case "channel":
      return "from chat";
    case "local_owner":
      return "on the computer";
    case "system":
      return "by the computer";
  }
}

/** A sign-in code as sites show it, without its spaces and dashes; null when it can't be one. */
export function signInCode(text: string): string | null {
  const code = text.replace(/[\s-]/g, "");
  return /^[A-Za-z0-9]{3,12}$/.test(code) ? code : null;
}

export function ApprovalCard({ approval: a, agent, myDeviceId, canDecide, draft, disabled = false, disabledReason, onDecide, onCode, onOpenTask }: ApprovalCardProps) {
  const id = useId();
  const now = useNow();
  const expired = a.expires_at !== null && now >= a.expires_at;
  disabled ||= expired;
  const isCode = a.kind === "login_code";
  const [mode, setMode] = useState<null | "deny" | "answer">(a.kind === "question" ? "answer" : null);
  // Held only while typed: it isn't the draft, which is kept on the phone.
  const [code, setCode] = useState("");
  const [localText, setLocalText] = useState("");
  const questions = a.questions ?? [];
  const [picked, setPicked] = useState(() => unpicked(questions));
  const answer = questions.length ? answered(questions, picked) : null;
  const text = draft?.text ?? localText;
  const setText = draft?.onChange ?? setLocalText;
  const [sending, setSending] = useState<null | "allow" | "deny" | "answer">(null);
  const [failure, setFailure] = useState<DecisionFailure | null>(null);
  const [accepted, setAccepted] = useState(false);
  /** The revision a refusal said was out of date; a newer one can be answered. */
  const [staleRevision, setStaleRevision] = useState<number | null>(null);
  const compose = useRef<HTMLDivElement>(null);
  const card = useRef<HTMLElement>(null);
  // Reconnecting settles a decision the computer hadn't confirmed. Once the
  // card can be answered again, still saying so would be stale.
  const wasDisabled = useRef(disabled);
  useEffect(() => {
    if (wasDisabled.current && !disabled) setFailure(f => f?.code === "outcome_unknown" ? null : f);
    wasDisabled.current = disabled;
  }, [disabled]);
  // Deny… can be pinned at the bottom of a sheet, well below where the reason opens.
  useEffect(() => {
    if (mode === "deny") compose.current?.scrollIntoView?.({ block: "nearest" });
  }, [mode]);

  const sendCode = async () => {
    const sent = signInCode(code);
    if (!sent || !onCode) return;
    setCode("");
    setSending("answer");
    setFailure(null);
    const refocus = focusAfterDecision(card.current);
    try {
      await onCode(sent);
      setAccepted(true);
      refocus();
    } catch (e) {
      const f = toFailure(e);
      if (f.code === "already_resolved" || f.code === "stale_revision" || f.code === "expired") setStaleRevision(a.revision);
      else { setFailure(f); setCode(sent); }
    } finally {
      setSending(null);
    }
  };

  const decide = async (kind: "allow" | "deny" | "answer") => {
    const d: ApprovalDecision =
      kind === "allow" ? { decision: "allow" }
        : kind === "deny" ? { decision: "deny", reason: questions.length ? null : text.trim() || null }
        : answer ? { decision: "answer", ...answer }
        : { decision: "answer", text: text.trim() };
    setSending(kind);
    setFailure(null);
    const refocus = focusAfterDecision(card.current);
    try {
      await onDecide(d);
      setAccepted(true);
      refocus();
    } catch (e) {
      const f = toFailure(e);
      if (f.code === "already_resolved" || f.code === "stale_revision" || f.code === "expired") setStaleRevision(a.revision);
      else setFailure(f);
    } finally {
      setSending(null);
    }
  };

  // Settled: show what happened, never an actionable stale card.
  if (a.state !== "pending" || staleRevision === a.revision) {
    const settled = settledText(a, myDeviceId);
    // Green/red only for what the agent reported; a decision it never
    // confirmed is shown as sent.
    const decided = a.state === "allowed" || a.state === "denied";
    const Icon = a.outcome === "allowed" ? CircleCheck : a.outcome === "denied" ? CircleX : decided ? SendHorizontal : CircleHelp;
    return (
      <article ref={card} tabIndex={-1} className="approval approval--settled" aria-labelledby={`${id}-t`}>
        <div className="approval__settled">
          <Icon aria-hidden className={`approval__settled-icon${a.outcome ? ` approval__settled-icon--${a.outcome}` : ""}`} strokeWidth={2.1} />
          <div>
            <p className="approval__settled-title" id={`${id}-t`}>
              {a.state === "pending" ? "Request changed" : settled.title}{a.resolved_at != null ? ` · ${clockTime(a.resolved_at)}` : ""}
            </p>
            <p className="approval__settled-what">{a.title}</p>
            {a.detail && !a.questions?.length && <p className="approval__detail"><ApprovalDetailText approval={a} /></p>}
            {a.shortened && <ExactRequest approval={a} />}
            <p className="approval__settled-meta">
              {a.state === "pending" ? "This request changed or was answered elsewhere. Refreshing…" : settled.meta}
              {a.resolved_at && (
                <>
                  {" · "}
                  <Age at={a.resolved_at} suffix=" ago" />
                </>
              )}
            </p>
            <ApprovalReason approval={a} />
          </div>
        </div>
      </article>
    );
  }

  const isQuestion = a.kind === "question";
  const unanswered = !(questions.length ? answer : text.trim());
  return (
    <article ref={card} tabIndex={-1} className="approval" aria-labelledby={`${id}-t`} aria-describedby={`${id}-d`} {...(canDecide && !accepted && { [OPEN_DECISION]: "" })}>
      <header className="approval__head">
        {isQuestion ? <MessageCircleQuestion aria-hidden strokeWidth={2.1} /> : isCode ? <KeyRound aria-hidden strokeWidth={2.1} /> : <ShieldAlert aria-hidden strokeWidth={2.1} />}
        <span className="approval__kicker">{isQuestion ? "Question" : isCode ? "Sign-in code" : "Approval needed"}</span>
        <Age at={a.requested_at} suffix=" ago" className="approval__age" />
      </header>
      <h3 className="approval__title" id={`${id}-t`}>
        {a.title}
      </h3>
      <p className="approval__who" id={`${id}-d`}>
        {onOpenTask ? (
          <button type="button" className="linklike" onClick={onOpenTask}>
            {agent}
          </button>
        ) : (
          agent
        )}{" "}
        wants to {ASK[a.kind]}
        {isQuestion ? ":" : "."}
      </p>
      {questions.length ? (
        <ApprovalQuestions questions={questions} picked={picked} onChange={setPicked} disabled={!canDecide || accepted || !!sending || disabled} />
      ) : a.detail && <p className="approval__detail"><ApprovalDetailText approval={a} /></p>}
      <ExactRequest approval={a} />
      <dl className="approval__facts">
        <div>
          <dt>Allows</dt>
          <dd>{a.scope}</dd>
        </div>
        {a.expires_at && (
          <div>
            <dt>Expires</dt>
            <dd>
              <Age at={a.expires_at} clock />
            </dd>
          </div>
        )}
      </dl>

      {accepted ? <p className="approval__note" role="status">Sent. Updating…</p> : !canDecide ? (
        <p className="approval__note">This device was not granted approvals. Answer it on the computer or from a device that can.</p>
      ) : isCode && mode !== "deny" ? (
        <form className="approval__compose" onSubmit={(e) => { e.preventDefault(); void sendCode(); }}>
          {onCode ? (
            <TextField label="Code" value={code} onChange={setCode} autoComplete="one-time-code" name="one-time-code"
              autoCapitalize="off" autoCorrect="off" spellCheck={false} disabled={!!sending || disabled}
              error={code.trim() && !signInCode(code) ? "A code is 3 to 12 letters or digits." : null}
              hint="Typed into the page, never shown to the agent." />
          ) : (
            <p className="approval__note">Open the task on this phone to type the code.</p>
          )}
          <div className="approval__actions">
            <Button variant="secondary" onClick={() => setMode("deny")} disabled={!!sending}>
              No code…
            </Button>
            {onCode && (
              <Button variant="primary" type="submit" pending={sending === "answer"} disabled={!!sending || disabled || !signInCode(code)}>
                {sending === "answer" ? "Sending" : "Fill code"}
              </Button>
            )}
          </div>
        </form>
      ) : mode ? (
        <div ref={compose} className="approval__compose">
          {!questions.length && <TextArea
            label={mode === "answer" ? "Your answer" : "Reason (optional)"}
            value={text}
            onChange={setText}
            rows={2}
            placeholder={mode === "answer" ? "Type an answer" : "Tell the agent why, so it can adjust"}
          />}
          <div className="approval__actions">
            {mode === "answer" ? (
              <>
                <Button variant="secondary" onClick={() => decide("deny")} pending={sending === "deny"} disabled={!!sending || disabled}>
                  Decline
                </Button>
                <Button variant="primary" onClick={() => decide("answer")} pending={sending === "answer"} disabled={!!sending || disabled || unanswered}
                  aria-describedby={unanswered && !disabled ? `${id}-unanswered` : undefined}>
                  {sending === "answer" ? "Sending" : "Send answer"}
                </Button>
              </>
            ) : (
              <>
                <Button variant="quiet" onClick={() => setMode(null)} disabled={!!sending}>
                  Back
                </Button>
                <Button variant="primary" onClick={() => decide("deny")} pending={sending === "deny"} disabled={!!sending || disabled}>
                  {sending === "deny" ? "Sending" : "Deny"}
                </Button>
              </>
            )}
          </div>
          {mode === "answer" && unanswered && !disabled && (
            <p className="approval__note" id={`${id}-unanswered`}>
              {questions.length > 1 ? "Answer each question to send." : questions.length ? "Choose an answer or write your own to send it." : "Write an answer to send it."}
            </p>
          )}
        </div>
      ) : (
        <div className="approval__actions">
          <Button variant="secondary" onClick={() => setMode("deny")} disabled={!!sending || disabled}>
            Deny…
          </Button>
          <Button variant="primary" onClick={() => decide("allow")} pending={sending === "allow"} disabled={!!sending || disabled}>
            {sending === "allow" ? "Sending" : "Allow"}
          </Button>
        </div>
      )}
      {expired ? <p className="approval__note">This request’s deadline has passed, so it can no longer be answered.</p>
        : disabled && disabledReason && disabledReason !== failure?.message && <p className="approval__note">{disabledReason}</p>}
      {(failure || a.delivery?.error) && (
        <p className="approval__error" role="alert">
          {failure?.message ?? a.delivery?.error?.message}
        </p>
      )}
    </article>
  );
}

/**
 * `state` records a successful hand-off; it does not prove agent receipt.
 * `outcome` is what the native agent later reported.
 * A reported outcome that disagrees with the decision is called out.
 */
export function settledText(a: Approval, me: string | null): { title: string; meta: string } {
  const who = byWhom(a.resolved_by, me);
  // A question is answered or declined; nothing else was allowed.
  const words = a.kind === "question" ? { allowed: "Answered", denied: "Declined" }
    : a.kind === "login_code" ? { allowed: "Code given", denied: "No code" }
    : { allowed: "Allowed", denied: "Denied" };
  const reported = a.outcome ? words[a.outcome] : null;
  switch (a.state) {
    case "allowed":
    case "denied": {
      const sent = `${words[a.state]}${who ? ` ${who}` : ""}`;
      if (!reported) return { title: words[a.state], meta: `${sent}. Sent to the agent` };
      if (a.state !== a.outcome) return { title: reported, meta: `${sent}, but the agent reported it ${reported.toLowerCase()}` };
      return { title: reported, meta: sent };
    }
    case "expired":
      return { title: "Expired", meta: "The request expired before anyone answered" };
    case "resolved_elsewhere":
      return { title: reported ? `${reported} elsewhere` : "Already resolved", meta: `Answered ${who || "elsewhere"}` };
    case "cancelled":
      return { title: "No longer needed", meta: "The run that asked has ended" };
    case "pending":
      return { title: "Waiting", meta: "" };
  }
}

function toFailure(e: unknown): DecisionFailure {
  if (e && typeof e === "object" && "error" in e) {
    const err = (e as { error: { code: string; message: string } }).error;
    return { code: err.code, message: err.message };
  }
  if (e instanceof Error) return { code: "not_sent", message: `Not sent: ${e.message}` };
  return { code: "internal", message: "Something went wrong" };
}

/**
 * Commands, paths, patterns and URLs read as code, exactly as written; the
 * rest reads as prose. A proposed file change reads as a diff, left out where
 * `brief` asks for a one-glance summary.
 */
export function ApprovalDetailText({ approval: a, brief = false }: { approval: Approval; brief?: boolean }) {
  if (!a.detail_lines?.length) return a.detail;
  const shown: ReactNode[] = [];
  let change: ApprovalDetailLine[] = [];
  const endChange = () => {
    if (change.length) shown.push(<ProposedChange key={shown.length} lines={change} />);
    change = [];
  };
  for (const line of a.detail_lines) {
    if (line.change) {
      if (!brief) change.push(line);
      continue;
    }
    endChange();
    shown.push(line.code && !line.label ? <code key={shown.length} className="approval__line approval__code">{line.text}</code>
      : <span key={shown.length} className="approval__line">{line.label && `${line.label}: `}{line.code ? <InlineCode className="approval__code">{line.text}</InlineCode> : line.text}</span>);
  }
  endChange();
  return shown;
}

/** Each line's mark and what a screen reader says for it; colour only repeats them. */
const CHANGE_MARK: Record<Exclude<ApprovalLineChange, "omitted">, { mark: string; said?: string }> = {
  removed: { mark: "−", said: "Removed" },
  added: { mark: "+", said: "Added" },
  unchanged: { mark: "" },
};

function ProposedChange({ lines }: { lines: ApprovalDetailLine[] }) {
  return <span className="approval__line approval__change" role="group" aria-label="Proposed change">
    {lines.map((line, i) => {
      if (line.change === "omitted") return <span key={i} className="approval__change-line approval__change-line--omitted"><span className="approval__change-mark" aria-hidden="true">⋯</span><span>{line.text}</span></span>;
      const { mark, said } = CHANGE_MARK[line.change ?? "unchanged"];
      return <span key={i} className={`approval__change-line approval__change-line--${line.change}`}>
        <span className="approval__change-mark" aria-hidden="true">{mark}</span>
        <code>{said && <span className="visually-hidden">{said}: </span>}{line.text}</code>
      </span>;
    })}
  </span>;
}

/** The whole input one tap away, with a note when the preview above it was cut. */
export function ExactRequest({ approval: a }: { approval: Approval }) {
  if (!a.raw_request) return null;
  return <>
    {a.shortened && <p className="approval__shortened">Shortened · the full request is under Exact request</p>}
    <details><summary className="linklike" style={{ minHeight: "var(--target)", display: "flex", alignItems: "center" }}>Exact request</summary>
      <pre className="approval__raw">{a.raw_request}</pre>
    </details>
  </>;
}

/** A retained decision is distinct from the native outcome. */
export function ApprovalReason({ approval }: { approval: Approval }) {
  const delivery = approval.delivery;
  if (!delivery || delivery.error) return null;
  const d = delivery.decision;
  if (d.decision === "answer") return <AnswerLines questions={approval.questions ?? []} answers={d.answers ?? []} text={d.text} />;
  return d.decision === "deny" && d.reason ? <p className="approval__settled-what">Reason: {d.reason}</p> : null;
}
