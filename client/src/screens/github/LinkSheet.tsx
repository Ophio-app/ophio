// SPDX-License-Identifier: Apache-2.0
//
// Review's GitHub row: what GitHub work a new task is for. Issues and pull
// requests can be picked from the open ones or typed as a number or link.

import { useEffect, useState } from "react";
import { CircleDot, GitPullRequest, GitPullRequestArrow, Layers, ListChecks, Search, SearchCheck, Unlink } from "lucide-react";
import { FOCUS_HINT, FOCUS_WORDS, focusesFor, parseNumber, useGithubItems, useGithubReviewSupported } from "../../app/github";
import { Button } from "../../design/Button";
import { Chip, Segmented } from "../../design/Chip";
import { TextField } from "../../design/Field";
import { ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import type { FindFocus, FindScope, GithubItemKind, GithubLink, GithubRepo, LaunchChoice } from "../../protocol/types";

type Step = "issue" | "pr" | "check" | "find";

export function LinkSheet({ open, onClose, projectId, repo, value, stackOn, fixWith, onPick }: {
  open: boolean;
  onClose: () => void;
  projectId: string;
  repo: GithubRepo | null;
  value: GithubLink | null;
  /** The source task's open pull request, for a Next up item. */
  stackOn: number | null;
  /** Who fixes what a finder files, until chosen in Review. */
  fixWith: LaunchChoice | null;
  onPick: (link: GithubLink | null) => void;
}) {
  const [step, setStep] = useState<Step | null>(null);
  useEffect(() => { if (open) setStep(null); }, [open]);
  const pick = (link: GithubLink | null) => { onPick(link); onClose(); };
  const kind = value?.type ?? null;
  return (
    <Sheet open={open} onClose={onClose} title={step ? STEP_TITLE[step] : "GitHub"} size="tall"
      leading={step ? <Button variant="quiet" size="sm" onClick={() => setStep(null)}>Back</Button> : undefined}>
      {step === null ? (
        <div className="group" role="radiogroup" aria-label="What it’s for on GitHub">
          <ListRow icon={Unlink} title="Not linked" subtitle="No pull request, nothing posted on GitHub." checked={kind === null} onClick={() => pick(null)} />
          <ListRow icon={GitPullRequestArrow} title="New pull request" subtitle="Opens a draft pull request for its branch and keeps its checks passing."
            checked={kind === "new_pr"} onClick={() => pick({ type: "new_pr" })} />
          <ListRow icon={CircleDot} title="Fix an issue" subtitle={value?.type === "issue" ? `Issue #${value.number}` : "Takes the issue’s title and branch."}
            checked={kind === "issue"} onClick={() => setStep("issue")} />
          <ListRow icon={GitPullRequest} title="Take over a pull request" subtitle={value?.type === "pr" ? `Pull request #${value.number}` : "Continues its branch and keeps its checks passing."}
            checked={kind === "pr"} onClick={() => setStep("pr")} />
          {stackOn !== null && (
            <ListRow icon={Layers} title={`Stack on PR #${stackOn}`} subtitle="Starts from its branch; its pull request targets it."
              checked={kind === "stack"} onClick={() => pick({ type: "stack", number: stackOn })} />
          )}
          <ListRow icon={Search} title="Find issues" subtitle="Reads in Plan, files an issue for each finding and queues its fix." checked={kind === "find"}
            disabled={!fixWith && kind !== "find"} onClick={() => setStep("find")} />
          <ListRow icon={ListChecks} title="Sort issues" subtitle="Sorts the open issues and pull requests. Nothing starts by itself."
            checked={kind === "triage"} onClick={() => pick({ type: "triage" })} />
          <ListRow icon={SearchCheck} title="Check a pull request" subtitle={value?.type === "check" ? `Pull request #${value.number}` : "Reviews its changes in Plan and reports back."}
            checked={kind === "check"} onClick={() => setStep("check")} />
        </div>
      ) : step === "find" ? (
        <FindStep value={value?.type === "find" ? value : null} fixWith={fixWith} onPick={pick} />
      ) : (
        <ItemStep list={step === "issue" ? "issues" : "prs"} projectId={projectId} repo={repo}
          onPick={(number) => pick(step === "issue" ? { type: "issue", number } : step === "pr" ? { type: "pr", number } : { type: "check", number })} />
      )}
    </Sheet>
  );
}

const STEP_TITLE: Record<Step, string> = { issue: "Fix an issue", pr: "Take over a pull request", check: "Check a pull request", find: "Find issues" };

function ItemStep({ list, projectId, repo, onPick }: { list: GithubItemKind; projectId: string; repo: GithubRepo | null; onPick: (n: number) => void }) {
  const { items, error, loading } = useGithubItems(projectId, list);
  const [typed, setTyped] = useState("");
  const number = parseNumber(typed, repo ?? undefined);
  const what = list === "issues" ? "issue" : "pull request";
  return (
    <div className="settings-stack">
      <TextField label={`The ${what}’s number or link`} value={typed} onChange={setTyped} inputMode="text" autoComplete="off" autoCapitalize="off" spellCheck={false}
        error={typed.trim() && number === null ? `Type #12, 12, or a link to a ${what} in ${repo ? `${repo.owner}/${repo.name}` : "this repository"}` : null} />
      <Button variant="primary" disabled={number === null} onClick={() => number !== null && onPick(number)}>
        {number === null ? "Use it" : `Use #${number}`}
      </Button>
      {error && <Notice tone="unknown">{error}</Notice>}
      {loading && !items && <div className="screen__loading" aria-busy="true" aria-label={`Loading open ${what}s`} />}
      {items && items.items.length > 0 && (
        <Section title={`Open ${what}s`}>
          <div className="group">
            {items.items.map((item) => (
              <ListRow key={item.number} icon={item.pr ? GitPullRequest : CircleDot} title={`#${item.number} ${item.title}`}
                subtitle={`@${item.author}${item.author_writes ? "" : " · can’t write to the repository"}`} onClick={() => onPick(item.number)} />
            ))}
          </div>
        </Section>
      )}
    </div>
  );
}

type ScopeKind = FindScope["scope"];
const SCOPE_WORDS: Record<ScopeKind, string> = { project: "Whole project", folder: "A folder", since_last_run: "Since its last run", pr: "One pull request" };

function FindStep({ value, fixWith, onPick }: { value: Extract<GithubLink, { type: "find" }> | null; fixWith: LaunchChoice | null; onPick: (link: GithubLink) => void }) {
  const [scope, setScope] = useState<ScopeKind>(value?.scope.scope ?? "project");
  const [path, setPath] = useState(value?.scope.scope === "folder" ? value.scope.path : "");
  const [pr, setPr] = useState(value?.scope.scope === "pr" ? String(value.scope.number) : "");
  const [focus, setFocus] = useState<FindFocus | null>(value?.focus ?? null);
  const focuses = focusesFor(useGithubReviewSupported());
  const prNumber = parseNumber(pr);
  const folder = path.trim().replace(/^\.?\/+/, "");
  const folderError = scope === "folder" && (folder === "" || folder.split("/").includes("..")) ? "A folder inside the project, like src/web" : null;
  const prError = scope === "pr" && prNumber === null ? "The pull request’s number" : null;
  const chosen: FindScope | null = scope === "project" ? { scope: "project" } : scope === "since_last_run" ? { scope: "since_last_run" }
    : scope === "folder" ? (folderError ? null : { scope: "folder", path: folder }) : prNumber === null ? null : { scope: "pr", number: prNumber };
  const fix = value?.fix_with ?? fixWith;
  return (
    <div className="settings-stack">
      <Segmented label="What it reads" value={scope} onChange={setScope}
        options={(["project", "folder", "since_last_run", "pr"] as const).map((s) => ({ value: s, label: SCOPE_WORDS[s] }))} />
      {scope === "folder" && <TextField label="Folder" value={path} onChange={setPath} autoComplete="off" autoCapitalize="off" spellCheck={false} error={path ? folderError : null} />}
      {scope === "pr" && <TextField label="Pull request number" value={pr} onChange={setPr} inputMode="numeric" error={pr ? prError : null} />}
      <div className="field">
        <span className="field__label" id="find-focus">Reviewer</span>
        <div className="github-focus" role="group" aria-labelledby="find-focus">
          <Chip selected={focus === null} onClick={() => setFocus(null)}>Default</Chip>
          {focuses.map((f) => (
            <Chip key={f} selected={focus === f} onClick={() => setFocus(f)}>{FOCUS_WORDS[f]}</Chip>
          ))}
        </div>
        <p className="settings-meta">{FOCUS_HINT[focus ?? "default"]}</p>
      </div>
      <p className="settings-meta">Review shows who finds and who fixes; change either there.</p>
      <Button variant="primary" disabled={!chosen || !fix} onClick={() => chosen && fix && onPick({
        type: "find", scope: chosen, ...(focus ? { focus } : {}), fix_with: fix,
        ...(value?.max_issues != null ? { max_issues: value.max_issues } : {}), ...(value?.fix_findings ? { fix_findings: value.fix_findings } : {}),
      })}>
        Use these
      </Button>
    </div>
  );
}
