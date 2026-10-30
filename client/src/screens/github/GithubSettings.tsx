// SPDX-License-Identifier: Apache-2.0
//
// Turning a project's GitHub on and off, from wherever it's asked: Set up
// GitHub under New in Work while it's off, GitHub settings among the
// project's doors, and the gear in Issues and Find & fix. One sheet says
// what's missing, holds the one switch, then the settings that matter once
// it's on. In a project whose folder isn't a Git repository, it offers the
// repositories in that folder as projects of their own.

import { useEffect, useId, useState } from "react";
import { FolderGit2, FolderPlus, GitPullRequest, KeyRound, Settings, ShieldCheck } from "lucide-react";
import { useApp, useCommand, useLink } from "../../app/context";
import { FIX_HINT, FIX_WORDS, MODE_HINT, useGithubProject } from "../../app/github";
import { PRODUCT_NAME } from "../../brand";
import { Button } from "../../design/Button";
import { Pill, Segmented } from "../../design/Chip";
import { TextField } from "../../design/Field";
import { ListRow, Section } from "../../design/ListRow";
import { Notice } from "../../design/Notice";
import { Sheet } from "../../design/Sheet";
import type { FixFindings, GithubProjectView, GithubSettings, Project, ProjectFolder } from "../../protocol/types";
import { NotAllowed, useMay } from "../../shell/NotAllowed";
import { Changing } from "../../design/StateLine";
import { Gate, type GithubSettingsControl, type Run, TURNING_ON, useGithubRun, useGithubSettings } from "./common";
import "../settings/settings.css";

export const EXPLAIN = `With GitHub on, an agent working in the project opens a draft pull request for its branch, keeps its checks passing and says on GitHub that it did the work. ${PRODUCT_NAME} uses the GitHub account signed in under Accounts.`;

/** Whether Work should offer to set GitHub up: it's off, or nobody is signed in to work as. */
export const needsSetup = (view: GithubProjectView): boolean => view.settings.mode === "off" || !view.login;

/** Directly under New while the project's GitHub isn't on: what's missing, and the way to fix it. */
export function GithubSetupRow({ project }: { project: Project }) {
  const { view } = useGithubProject(project.project_id);
  const [open, setOpen] = useState(false);
  const repo = view?.repo;
  const inside = view?.repo_folders?.[0];
  const subtitle = !view ? ""
    : project.vcs?.kind !== "git" ? (inside ? `Its folder isn’t a Git repository, but ${inside.name} inside it is` : "Its folder isn’t a Git repository")
    : !view.login ? "Sign in to GitHub under Accounts first"
    : !repo ? view.problem ?? "This project isn’t on GitHub"
      : `Turn it on for ${repo.owner}/${repo.name} so agents open pull requests`;
  return <>
    {view && needsSetup(view) && <ListRow icon={GitPullRequest} title="Set up GitHub" subtitle={subtitle} chevron onClick={() => setOpen(true)} />}
    <GithubSheet project={project} open={open} onClose={() => setOpen(false)} />
  </>;
}

/** GitHub settings among the project's doors, for turning it off as easily as on. */
export function GithubSettingsRow({ project }: { project: Project }) {
  const [open, setOpen] = useState(false);
  return <>
    <ListRow icon={Settings} title="GitHub settings" subtitle="Turn GitHub on or off for this project" chevron onClick={() => setOpen(true)} />
    <GithubSheet project={project} open={open} onClose={() => setOpen(false)} />
  </>;
}

/** The settings sheet with its own read of the project's GitHub, made only while it's open. */
export function GithubSheet({ project, open, onClose }: { project: Project; open: boolean; onClose: () => void }) {
  const { view, error, reload } = useGithubProject(open ? project.project_id : null);
  const { busy, run } = useGithubRun();
  const settings = useGithubSettings(view, reload);
  const act: Run = (action, label, done) => run(action, label, done).then((ok) => { reload(); return ok; });
  return <SettingsSheet open={open} onClose={onClose} project={project} view={view} error={error} busy={busy} run={act} reload={reload} settings={settings} />;
}

/** Whether GitHub is on, as the computer is asked it. */
export const turnGithub = (settings: GithubSettingsControl, on: boolean) => settings.set({ ...settings.value, mode: on ? "drafts" : "off" }, {
  label: on ? TURNING_ON : "Turning off GitHub", what: "whether GitHub is on", done: on ? "GitHub is on" : "GitHub is off",
});

export function SettingsSheet({ open, onClose, project, view, error, busy, run, reload, settings }: {
  open: boolean; onClose: () => void; project: Project; view: GithubProjectView | null; error: string | null;
  busy: string | null; run: Run; reload: () => void; settings: GithubSettingsControl;
}) {
  const { profile, ui } = useApp();
  const online = useLink().state === "online";
  const admin = useMay("admin");
  const repo = view?.repo;
  const disabled = !online || !admin;
  const on = settings.inForce.mode !== "off";
  const title = useId(), hint = useId();
  const tryAgain = <Button size="sm" disabled={!online} onClick={reload}>Try again</Button>;
  const accounts = () => { onClose(); ui.push({ screen: "accounts" }); };
  return (
    <Sheet open={open} onClose={onClose} title="GitHub settings" subtitle={repo ? `${repo.owner}/${repo.name}` : project.name} size="tall">
      <div className="settings-stack">
        <Gate what="change GitHub settings">
          {error ? <Notice tone="unknown" action={tryAgain}>{error}</Notice>
            : !view ? <div className="screen__loading" aria-busy="true" aria-label="Loading GitHub" />
              : <>
                {!admin && <NotAllowed what="change GitHub settings" permission="manage" />}
                {!view.login && (
                  <Notice tone="warn" icon={KeyRound} title="No GitHub account" action={<Button size="sm" onClick={accounts}>Open Accounts</Button>}>
                    Sign in to GitHub under Accounts on {profile.name} first. The agent works as that account.
                  </Notice>
                )}
                {view.problem && <Notice tone="unknown" action={tryAgain}>{view.problem}</Notice>}
                {!!view.repo_folders?.length && <RepoFolders folders={view.repo_folders} onClose={onClose} />}
                <label className="github-switch">
                  <input type="checkbox" role="switch" checked={on} disabled={disabled || !repo} aria-labelledby={title} aria-describedby={hint}
                    onChange={(e) => void turnGithub(settings, e.target.checked)} />
                  <span className="github-switch__text">
                    <span className="github-switch__title" id={title}>Use GitHub for this project</span>
                    <span className="settings-meta" id={hint}>
                      {on ? MODE_HINT[settings.inForce.mode]
                        : repo ? `${MODE_HINT.off} Turn it on and agents open draft pull requests, keep their checks passing and say on GitHub that they did the work; you merge them.`
                          : project.vcs?.kind !== "git" ? `${MODE_HINT.off} It can turn on in a project whose folder is a Git repository with its origin on github.com.`
                            : `${MODE_HINT.off} It can turn on once there’s a GitHub account to work as and the project’s origin is on github.com.`}
                    </span>
                  </span>
                </label>
                {settings.turning && <Changing to={settings.turning === "off" ? "turning GitHub off" : "turning GitHub on"} />}
                {repo && <RepoCard view={view} busy={busy} run={run} />}
                {repo && on && <Details settings={settings} disabled={disabled} />}
                {!on && <p className="settings-meta">{EXPLAIN}</p>}
              </>}
        </Gate>
      </div>
    </Sheet>
  );
}

/** The repositories in a project's folder that isn't one: each opens as its own project, added first if it isn't one yet. */
function RepoFolders({ folders, onClose }: { folders: ProjectFolder[]; onClose: () => void }) {
  const { ui } = useApp();
  const command = useCommand();
  const online = useLink().state === "online";
  const admin = useMay("admin");
  const [adding, setAdding] = useState(false);
  const open = (projectId: string) => { onClose(); ui.selectProject(projectId); };
  const add = async (folder: ProjectFolder) => {
    setAdding(true);
    const r = await command({ command: "add_project", host_path: folder.path, name: null }, { label: "Add project", success: "Project added" });
    setAdding(false);
    if (r?.result?.kind === "project_added") open(r.result.project_id);
  };
  return (
    <div className="group" role="group" aria-label="Repositories in this folder">
      {folders.map((f) => f.project_id
        ? <ListRow key={f.path} icon={FolderGit2} title={`Open ${f.name}`} subtitle="It’s a project already: set up GitHub there" chevron onClick={() => open(f.project_id!)} />
        : <ListRow key={f.path} icon={FolderPlus} title={`Add ${f.name} as a project`} subtitle="Then set up GitHub there" chevron
          disabled={!online || !admin || adding} onClick={() => void add(f)} />)}
    </div>
  );
}

const PROTECTING = "Protecting the default branch";

function RepoCard({ view, busy, run }: { view: GithubProjectView; busy: string | null; run: Run }) {
  const online = useLink().state === "online";
  const admin = useMay("admin");
  const [protecting, setProtecting] = useState(false);
  const repo = view.repo!;
  const branch = repo.default_branch ?? "main";
  return (
    <article className="settings-card github-repo settings-stack" aria-label="Repository">
      <p className="account-row__meta">
        <Pill>{repo.private ? "Private" : "Public"}</Pill>
        {view.login && <span className="settings-meta">Works as @{view.login}</span>}
        {!repo.owned && <span className="settings-meta">Not your repository: the agent asks before posting</span>}
      </p>
      <p className="github-protect">
        {repo.protected === true ? <><ShieldCheck aria-hidden className="github-protect__icon" />{`${branch} is protected`}</>
          : repo.protected === false ? `${branch} isn’t protected, so nothing stops a push that skips a pull request.`
            : `Whether ${branch} is protected couldn’t be read.`}
      </p>
      {repo.protected === false && repo.admin && !protecting && (
        <Button size="sm" disabled={!online || !admin || !!busy} onClick={() => setProtecting(true)}>Protect {branch}</Button>
      )}
      {protecting && (
        <div className="settings-stack">
          <p>GitHub will refuse any change to {branch} that doesn’t come through a pull request, admins’ included. Where the repository runs checks on pull requests, they must pass first.</p>
          <p className="settings-meta">It needs no approving review, so the agent’s own pull requests can still merge once their checks pass. Checks and rules {branch} already has are kept. Change or remove it in the repository’s settings on GitHub.</p>
          <div className="github-list__actions">
            <Button variant="quiet" size="sm" onClick={() => setProtecting(false)}>Not now</Button>
            <Button variant="primary" size="sm" disabled={!online || !!busy} pending={busy === PROTECTING}
              onClick={() => void run({ action: "protect_branch", project_id: view.project_id }, PROTECTING, `${branch} protected`).then((ok) => { if (ok) setProtecting(false); })}>
              Protect {branch}
            </Button>
          </div>
        </div>
      )}
    </article>
  );
}

const LIMIT = { findings: [1, 20] } as const;
const inRange = (text: string, [lo, hi]: readonly [number, number]) => /^\d{1,3}$/.test(text.trim()) && +text >= lo && +text <= hi;

/** The settings that matter once GitHub is on. */
function Details({ settings, disabled }: { settings: GithubSettingsControl; disabled: boolean }) {
  // Each choice shows at once; the computer gets them one after another.
  const s = settings.value;
  const save = (next: GithubSettings, what: string, done?: string) => settings.set(next, { label: "Saving GitHub settings", what, ...(done ? { done } : {}) });
  // What's typed stays put through a refusal; only the computer's own change replaces it.
  const kept = settings.inForce;
  const [prefix, setPrefix] = useState(kept.branch_prefix);
  const [findings, setFindings] = useState(String(kept.workflow.findings_per_run));
  useEffect(() => { setPrefix(kept.branch_prefix); setFindings(String(kept.workflow.findings_per_run)); },
    [kept.branch_prefix, kept.workflow.findings_per_run]);
  const labelHint = useId(), checkHint = useId();
  const prefixError = prefix.length > 40 ? "40 characters at most" : /\s|\.\.|[~^:?*[\\]/.test(prefix) ? "Git doesn’t accept that in a branch name" : null;
  const findingsError = inRange(findings, LIMIT.findings) ? null : "1 to 20";
  const changed = prefix !== s.branch_prefix || findings !== String(s.workflow.findings_per_run);
  return <>
    <Section title="Branches and pull requests">
      <div className="settings-card settings-stack">
        <TextField label="Branch prefix" value={prefix} onChange={setPrefix} autoComplete="off" autoCapitalize="off" spellCheck={false}
          hint="Branches the agent makes start with this." error={prefixError} disabled={disabled} />
        <TextField label="Issues per review" value={findings} onChange={setFindings} inputMode="numeric"
          hint="What a new review in Find & fix starts at, and the most a review files when nobody chose." error={findingsError} disabled={disabled} />
        <Button variant="primary" disabled={disabled || !changed || !!prefixError || !!findingsError}
          onClick={() => void save({ ...s, branch_prefix: prefix, workflow: { ...s.workflow, findings_per_run: +findings } }, "the branch prefix and issues per review", "GitHub settings saved")}>
          Save
        </Button>
        <label className="settings-check">
          <input type="checkbox" checked={s.label} disabled={disabled} aria-describedby={labelHint}
            onChange={(e) => void save({ ...s, label: e.target.checked }, "whether what the agent opens is labelled")} />
          Label what the agent opens “agent”
        </label>
        <p className="settings-meta" id={labelHint}>Only on repositories you own. The label is made the first time it’s used.</p>
      </div>
    </Section>
    <Section title="Reviews">
      <div className="settings-card settings-stack">
        <Segmented label="After a review files issues" value={s.workflow.fix_findings}
          onChange={(v: FixFindings) => void save({ ...s, workflow: { ...s.workflow, fix_findings: v } }, "what follows a review")}
          options={(["by_itself", "ask_first", "file_only"] as const).map((v) => ({ value: v, label: FIX_WORDS[v], disabled }))} />
        <p className="settings-meta">What a new review in Find & fix starts at. {FIX_HINT[s.workflow.fix_findings]} Issues anyone else wrote never start by themselves.</p>
        <label className="settings-check">
          <input type="checkbox" checked={s.workflow.check_before_merge} disabled={disabled} aria-describedby={checkHint}
            onChange={(e) => void save({ ...s, workflow: { ...s.workflow, check_before_merge: e.target.checked } }, "whether changes are checked before merging")} />
          Check before merging
        </label>
        <p className="settings-meta" id={checkHint}>When the agent’s pull request passes its checks, its changes are reviewed once, without changing files, before it merges or is handed to you: by the reviewer chosen for the issue, else the reviewer whose issue it fixes, else the task’s own agent.</p>
      </div>
    </Section>
  </>;
}
