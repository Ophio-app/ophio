// SPDX-License-Identifier: Apache-2.0
//
// One review result: a headline in plain sentences, then four separate
// facts (the review, the pull request's checks, what each confirmed problem
// rests on, and whether the person must approve this exact commit), then
// the details behind them for whoever wants them: each finding with its
// verdict and what became of it, the set-aside ones with why, the agents,
// the review rules followed and the commit covered. Shown in a pull
// request's sheet and on the result of the task that made it; a finder's
// own task shows what it filed and set aside. The words come from
// app/review.ts, so every screen says the same thing.

import { useId, useState } from "react";
import { CircleDashed, CircleSlash, GitMerge, ShieldAlert, ShieldCheck } from "lucide-react";
import { useHost } from "../../app/context";
import {
  MAX_ROUNDS, SET_ASIDE_WORDS, confirmed, counted, dispositionWords, findingWhere, numberWord, reviewResult, reviewerReasons, rulesWords, uncheckedReasons,
  setAside, shortCommit, taskReviewInput, unchecked, verdictWords, cap, areaWords, type Fact, type ReviewResult,
} from "../../app/review";
import { Button } from "../../design/Button";
import { Pill } from "../../design/Chip";
import { ListRow } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import type { ApprovalNeed, LaunchChoice, PrReview, PrSummary, ReviewFinding, Task, TaskReview } from "../../protocol/types";
import { agentLabel } from "../talk/intent";
import "./github.css";

/** A task's pull request review, worked out once for the card and the merge button. */
export function taskReviewOf(task: Task | null | undefined, pr?: PrSummary | null): ReviewResult | null {
  if (!task) return null;
  const input = taskReviewInput(task, pr);
  return input ? reviewResult(input) : null;
}

const FACTS: { key: "review" | "checks" | "evidence" | "approval"; label: string }[] = [
  { key: "review", label: "Review" },
  { key: "checks", label: "Checks" },
  { key: "evidence", label: "Evidence" },
  { key: "approval", label: "Your approval" },
];

/**
 * A pull request's review result, from its task. Nothing when the computer
 * recorded no review and no need for approval.
 */
export function ReviewResultCard({ task, pr }: { task: Task; pr?: PrSummary | null }) {
  const result = taskReviewOf(task, pr);
  const github = task.github;
  const heading = useId();
  if (!result || !github) return null;
  const review = github.review ?? null;
  const escalated = result.needsYou === "escalated" ? review?.escalated?.trim() : null;
  const tone = result.needsYou ? "warn" : result.review.tone === "ok" && result.checks.tone !== "stop" ? "ok" : "neutral";
  return (
    <section className={`review-result review-result--${tone}`} aria-labelledby={heading}>
      <h3 id={heading} className="review-result__eyebrow">Review result</h3>
      <p className="review-result__headline">{result.headline.join(" ")}</p>
      {escalated && (
        <Notice tone="warn" report={false} icon={ShieldAlert} title="Why it needs you">{escalated}</Notice>
      )}
      <dl className="review-facts">
        {FACTS.map(({ key, label }) => <FactRow key={key} label={label} fact={result[key]} />)}
      </dl>
      <ReviewDetails review={review} approval={github.approval ?? null} head={pr?.head ?? github.pr?.head ?? null} />
    </section>
  );
}

function FactRow({ label, fact }: { label: string; fact: Fact }) {
  return (
    <div className="review-fact">
      <dt className="review-fact__label">{label}</dt>
      <dd className="review-fact__word"><Pill tone={fact.tone}>{fact.word}</Pill></dd>
      <dd className="review-fact__text">{fact.text}</dd>
    </div>
  );
}

/** The agents a review used, by name, model and effort. */
function useAgent(choice: LaunchChoice | null | undefined): string | null {
  const harnesses = useHost((s) => s.harnesses);
  return choice ? agentLabel(harnesses, { harness: choice.harness, model: choice.model, effort: choice.effort }) : null;
}

/** Everything behind the headline, folded away until asked for. */
function ReviewDetails({ review, approval, head }: { review: PrReview | null; approval: ApprovalNeed | null; head: string | null }) {
  const reviewer = useAgent(review?.reviewer);
  const verifier = useAgent(review?.verifier);
  const findings = review?.findings ?? [];
  const reasons = approval ? reviewerReasons(approval) : [];
  const notChecked = approval ? uncheckedReasons(approval) : [];
  const covered = review?.reviewed_head ?? null;
  const rounds = review?.round ?? 0;
  return (
    <details className="review-more">
      <summary>What the review found and followed</summary>
      <div className="review-more__body">
        <FindingGroups findings={findings} reviewed={!!covered} />
        {approval && (
          <div className="review-more__part">
            <h4>Why it needs your approval</h4>
            <ul className="review-more__list">
              {approval.areas.flatMap((a, i) => { const w = areaWords(a); return w ? [<li key={i}>It changes {w}.</li>] : []; })}
              {reasons.map((r) => <li key={r}>The reviewer said: {r}</li>)}
              {notChecked.map((r) => <li key={r}>{r}</li>)}
            </ul>
            {(approval.paths ?? []).length > 0 && (
              <p className="settings-meta">Files that made it sensitive: {(approval.paths ?? []).join(", ")}.</p>
            )}
            <p className="settings-meta">Approving covers commit {shortCommit(approval.head)} only. A newer commit needs a fresh look.</p>
          </div>
        )}
        <div className="review-more__part">
          <h4>What it covered</h4>
          <ul className="review-more__list">
            {covered ? <li>It reviewed the change up to commit {shortCommit(covered)}.</li> : <li>No round of review has finished.</li>}
            {covered && head && head !== covered && <li>The pull request has newer commits, up to {shortCommit(head)}. {review?.reviewing_head === head ? "A review of them is under way." : "They haven’t been reviewed."}</li>}
            {covered && head === covered && <li>That is the pull request’s newest commit.</li>}
            <li>{cap(counted(rounds, "round"))} of review {rounds === 1 ? "has" : "have"} been used, of at most {numberWord(MAX_ROUNDS)}.</li>
            {(review?.not_run ?? []).map((n) => <li key={n}>This check was not run: {n.replace(/[.]+$/, "")}.</li>)}
          </ul>
        </div>
        <div className="review-more__part">
          <h4>Who reviewed it</h4>
          <ul className="review-more__list">
            <li>{reviewer ? `Reviewed by ${reviewer}.` : "The reviewer wasn’t recorded."}</li>
            <li>{verifier ? `Each finding was checked by a separate agent: ${verifier}.` : findings.length ? "The agent that checked the findings wasn’t recorded." : "No findings needed checking."}</li>
          </ul>
        </div>
        <div className="review-more__part">
          <h4>Review rules</h4>
          {rulesWords(review?.rules).map((line) => <p key={line} className="settings-meta">{line}</p>)}
        </div>
      </div>
    </details>
  );
}

/** Findings by what became of them: confirmed first, then still being checked, then set aside with why. */
function FindingGroups({ findings, filedHere = false, reviewed = true }: { findings: ReviewFinding[]; filedHere?: boolean; reviewed?: boolean }) {
  const [shown, setShown] = useState<ReviewFinding | null>(null);
  const sure = findings.filter(confirmed);
  const waiting = findings.filter(unchecked);
  const aside = findings.filter(setAside);
  const row = (f: ReviewFinding, i: number) => {
    const where = findingWhere(f);
    const sideways = setAside(f);
    return (
      <ListRow key={`${f.title}-${i}`} icon={sideways ? CircleSlash : f.verdict === "confirmed" ? ShieldCheck : CircleDashed} title={f.title} chevron onClick={() => setShown(f)}
        subtitle={<>
          {sideways ? (f.reason?.trim() ? cap(f.reason.trim()) : "The checking agent gave no reason.") : verdictWords(f.verdict, f.evidence)}
          {where && <span className="github-item__note">{where}</span>}
        </>}
        meta={sideways
          ? <Pill tone="neutral">{f.verdict === "uncertain" ? SET_ASIDE_WORDS.uncertain : f.verdict === "rejected" ? SET_ASIDE_WORDS.rejected : "Set aside"}</Pill>
          : f.verdict === "confirmed" && <Pill tone={f.disposition.state === "corrected" ? "ok" : f.disposition.state === "open" ? "warn" : "neutral"}>{dispositionWords(f.disposition)}</Pill>} />
    );
  };
  if (!findings.length && !reviewed) return <p className="settings-meta">No round of review has finished, so there is nothing to report.</p>;
  if (!findings.length) return <p className="settings-meta">It found nothing to report.{filedHere ? "" : " That doesn’t prove the change is right; it means this review didn’t find a problem."}</p>;
  return <>
    {sure.length > 0 && (
      <div className="review-more__part">
        <h4>{sure.length === 1 ? "Confirmed problem" : `Confirmed problems (${sure.length})`}</h4>
        <div className="group">{sure.map(row)}</div>
      </div>
    )}
    {waiting.length > 0 && (
      <div className="review-more__part">
        <h4>Not checked yet</h4>
        <div className="group">{waiting.map(row)}</div>
      </div>
    )}
    {aside.length > 0 && (
      <div className="review-more__part">
        <h4>{aside.length === 1 ? "Set aside" : `Set aside (${aside.length})`}</h4>
        <p className="settings-meta">A separate agent checked {aside.length === 1 ? "this one and couldn’t show it’s" : "these and couldn’t show they’re"} a real problem in this change, so nothing was done about {aside.length === 1 ? "it" : "them"}. {aside.length === 1 ? "It stays" : "They stay"} here, not on GitHub.</p>
        <div className="group">{aside.map(row)}</div>
      </div>
    )}
    {shown && <FindingSheet finding={shown} onClose={() => setShown(null)} />}
  </>;
}

/** One finding in full: the failure, the verdict and what it rests on, and what became of it. */
export function FindingSheet({ finding: f, onClose }: { finding: ReviewFinding; onClose: () => void }) {
  const where = findingWhere(f);
  const aside = setAside(f);
  const parts: [string, string | null | undefined][] = [
    ["What makes it happen", f.scenario], ["What should happen", f.expected], ["What happens instead", f.actual],
  ];
  return (
    <Sheet open onClose={onClose} title={f.title} subtitle={aside ? "Set aside" : f.verdict === "confirmed" ? dispositionWords(f.disposition) : "Not checked yet"}>
      <div className="settings-stack">
        {(f.severity || where) && (
          <p className="settings-meta">
            {f.severity && <Pill tone={f.severity === "high" ? "stop" : f.severity === "medium" ? "warn" : "neutral"}>{cap(f.severity)}</Pill>}
            {where && <> {where}</>}
          </p>
        )}
        <Notice tone={f.verdict === "confirmed" && !aside ? "info" : "unknown"} report={false} icon={aside ? CircleSlash : f.verdict === "confirmed" ? ShieldCheck : CircleDashed}
          title={aside ? `Set aside: ${f.verdict === "uncertain" ? SET_ASIDE_WORDS.uncertain.toLowerCase() : f.verdict === "rejected" ? SET_ASIDE_WORDS.rejected.toLowerCase() : "not acted on"}` : verdictWords(f.verdict, f.evidence)}>
          {f.reason?.trim() || (f.verdict ? "The checking agent gave no more detail." : "A separate agent hasn’t checked this one yet.")}
          {f.verdict === "confirmed" && f.evidence === "source_reasoning" && " It rests on reading the code: nothing was run to show it."}
        </Notice>
        {parts.filter(([, v]) => v?.trim()).map(([label, v]) => (
          <div key={label}>
            <h4 className="review-more__label">{label}</h4>
            <p className="review-more__text">{v}</p>
          </div>
        ))}
        <p className="settings-meta">
          {f.round ? `Found in round ${numberWord(f.round)}` : "Found"}{f.revision ? ` at commit ${shortCommit(f.revision)}` : ""}.
        </p>
      </div>
    </Sheet>
  );
}

const ROLE_TITLE: Record<TaskReview["role"], string> = { find: "What the review found", verify: "What the check made of each finding", recheck: "What the recheck found" };

/**
 * What a review task's findings came to: which were filed or are being
 * fixed, and which were set aside and why. Nothing for a task with none.
 */
export function TaskFindings({ review }: { review: TaskReview | null | undefined }) {
  const heading = useId();
  const findings = review?.findings ?? [];
  if (!review || !findings.length) return null;
  const sure = findings.filter(confirmed);
  const filed = sure.filter((f) => f.disposition.state === "filed").length;
  const aside = findings.filter(setAside).length;
  const waiting = findings.filter(unchecked).length;
  const lines = [
    sure.length ? `${cap(counted(sure.length, "confirmed problem"))}${filed ? `, ${filed === sure.length ? (filed === 1 ? "filed as an issue" : "all filed as issues") : `${numberWord(filed)} filed as ${filed === 1 ? "an issue" : "issues"}`}` : ""}.` : "No problem was confirmed.",
    waiting ? `${cap(counted(waiting, "possible problem"))} ${waiting === 1 ? "is" : "are"} still being checked.` : null,
    aside ? `${cap(counted(aside, "possible problem"))} ${aside === 1 ? "was" : "were"} set aside after a separate agent checked ${aside === 1 ? "it" : "them"}.` : null,
  ].filter(Boolean).join(" ");
  return (
    <section className="review-result review-result--neutral" aria-labelledby={heading}>
      <h3 id={heading} className="review-result__eyebrow">{ROLE_TITLE[review.role]}</h3>
      <p className="review-result__headline">{lines}</p>
      <details className="review-more">
        <summary>Each finding and why</summary>
        <div className="review-more__body">
          <FindingGroups findings={findings} filedHere />
          {review.rules && (
            <div className="review-more__part">
              <h4>Review rules</h4>
              {rulesWords(review.rules).map((line) => <p key={line} className="settings-meta">{line}</p>)}
            </div>
          )}
        </div>
      </details>
    </section>
  );
}

/**
 * Before the person's merge that is also their approval: what it affects
 * and the exact commit, so the unlock that follows approves only that.
 */
export function ApproveSheet({ approval, number, base, onClose, onApprove }: {
  approval: ApprovalNeed; number: number; base: string; onClose: () => void; onApprove: () => void;
}) {
  const areas = approval.areas.flatMap((a) => { const w = areaWords(a); return w ? [w] : []; });
  const reasons = reviewerReasons(approval);
  const notChecked = uncheckedReasons(approval);
  const commit = shortCommit(approval.head);
  return (
    <Sheet open onClose={onClose} title={`Approve commit ${commit}?`} subtitle={`Pull request #${number}`}
      footer={<>
        <Button variant="secondary" onClick={onClose}>Not now</Button>
        <Button variant="primary" icon={GitMerge} onClick={onApprove}>Approve and merge</Button>
      </>}>
      <div className="settings-stack">
        <p>This change affects:</p>
        <ul className="review-more__list">
          {areas.map((a) => <li key={a}>{cap(a)}</li>)}
          {reasons.map((r) => <li key={r}>What the reviewer raised: {r}</li>)}
          {notChecked.map((r) => <li key={r}>{r}</li>)}
        </ul>
        <p>Approving merges commit {commit} into {base}, and nothing newer. If another commit arrives first, it needs your approval again.</p>
        <p className="settings-meta">No agent can approve this for you. Your phone’s unlock confirms it.</p>
      </div>
    </Sheet>
  );
}
