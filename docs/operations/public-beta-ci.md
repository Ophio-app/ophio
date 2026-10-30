<!-- SPDX-License-Identifier: Apache-2.0 -->
# Public beta contribution and CI safety

The initial public beta source must go into a **new repository**, not a public
visibility change to the private development repository. The beta starts with
GitHub Actions disabled, no workflow files, and no access to private build
runners. Contributions can be discussed and reviewed without automatically
executing their code on a maintainer's computer.

This document is an operator procedure, not permission to create a repository,
publish source, change live settings, or start a runner. Publication needs the
owner's separate approval. It does not enable or qualify the report investigator.

## What keeps the two repositories separate

Private development uses repository-scoped runners. Leave those registrations
attached only to the private repository. Never register a public-repository
runner on a maintainer's workstation or grant the public repository access to a
private runner group. Do not copy runner credentials, repository secrets,
environments, release credentials, or automation integrations into the public
repository.

The private workflow's job conditions check the private repository's identity
and reject fork pull requests. These conditions only protect against accidental
copies of the unchanged workflow. **They do not protect against someone who can
edit workflow YAML.** A contributor can change runner labels, remove conditions,
or add another workflow. Labels, branch names, contributor approval prompts,
and a check inside a job are not an isolation boundary.

Repository-scoped runner registration and the repository's access policy are
outside contributor-controlled files. Keeping the development repository private
and leaving public Actions disabled are the initial beta's boundaries. Private
repository access itself is trusted access; do not invite unknown contributors
there as a substitute for public contribution handling.

## Before importing the first public source

Perform these steps only after repository creation and publication have been
separately approved. Use the normal protected GitHub credential process; never
put a token in a command argument, URL, source file, or exported tree.

1. Create a new, empty repository at the exact owner and name approved for the
   beta. Do not transfer or change the visibility of the development repository.
   Do not push anything yet.
2. Set the variable below to that new repository's `owner/name`. Verify the
   returned name, numeric identity, and visibility before making any change.
   The numeric identity must differ from the private development repository.

   ```sh
   PUBLIC_REPOSITORY='APPROVED_OWNER/APPROVED_NEW_REPOSITORY'
   gh api "repos/$PUBLIC_REPOSITORY" \
     --jq '{full_name, id, visibility, default_branch}'
   ```

3. Disable Actions for the entire new repository before its first import. Do
   not merely disable a named workflow: a pull request can add another one.
   In Settings, Actions, General, select **Disable actions**, or use this API
   operation after the separate approval to change live settings.

   ```sh
   printf '%s\n' '{"enabled":false}' | \
     gh api --method PUT "repos/$PUBLIC_REPOSITORY/actions/permissions" --input -
   gh api "repos/$PUBLIC_REPOSITORY/actions/permissions" --jq '.enabled'
   ```

   The final result must be `false`. A failed or inaccessible settings request
   is a blocker, not evidence that Actions is disabled.
4. Check Settings, Actions, Runners and any organization runner-group access.
   The new repository must have no registered or inherited private runners.
   Check the API result too; `total_count` must be zero. If pagination is needed,
   inspect every page rather than trusting the first one.

   ```sh
   gh api "repos/$PUBLIC_REPOSITORY/actions/runners?per_page=100" \
     --jq '{total_count, runners: [.runners[] | {name, status}]}'
   ```

5. Confirm that repository and environment secrets, deployment environments,
   organization-inherited secrets, installed GitHub Apps, and webhooks do not
   give this repository access to private builds, signing, deployment, or a
   computer-use agent. Check external services that poll repositories as well;
   absence of webhooks does not prove that no bot is watching. Public issue
   text and submitted tests remain untrusted. Do not route them to a fixer or
   report-investigation worker as part of this import.
6. Build the clean source export with the approved export helper. It must omit
   `.github/workflows` entirely, with no opt-in to retain private workflows.
   Run the privacy/source audit and this independent check **from a trusted
   checkout**, passing the plain exported directory as its argument.

   ```sh
   python3 scripts/public_ci_policy.py /absolute/path/to/exported-tree
   ```

   The check rejects workflow files, empty workflow directories, and symlinks
   at the workflow directory or its `.github` parent. It does not inspect live
   GitHub settings, audit source privacy, or certify that building the code is
   safe. Do not run a contributor's replacement for this checker.
7. After reviewing the source audit and tree check, import only the approved
   exported files as fresh history, not development branches, tags, or private
   Git history. Recheck Actions is disabled and runner access is empty after
   import. Confirm that no workflow runs started. Keep those checks in the
   publication receipt before inviting outside contributors.

The source export alone cannot prove this checklist passed. Until the actual
new repository has been checked, its live settings are **unverified**.

## Reviewing contributions without automatic execution

Opening an issue or pull request must not start a job on the private build
host, a signing system, or a report investigator. Review text and diffs first.
Do not fetch a stranger's branch into private CI and assume that renaming it,
approving its workflow, or opening a same-repository pull request makes it
trusted. Test code, package install hooks, build scripts, dependency changes,
and workflow edits can all execute commands.

Any manual execution of an untrusted change needs a separately approved,
isolated environment with no private credentials, host home directory, host
services, signing material, or reused writable build state. There is no safe
fallback to the workstation when that environment is unavailable. A container
on a sensitive workstation is not, by itself, sufficient separation.

Release signing and publication stay outside contributor jobs. A passing check
or downloadable CI artifact is not authority to sign, update, or publish it.
Release operators must review the selected source and preserve the separate
signing and owner-approval requirements. The initial public beta adds no
release workflow and grants no release credentials.

## If public CI is introduced later

Treat enabling Actions as a new security change, not a variable toggle in the
private workflow. Keep the initial workflow-free export policy until that change
has its own approval and tests. Use a separately reviewed GitHub-hosted or
truly disposable isolated runner setup. Do not enable paid service usage without
an approved budget, and do not fall back to private runners if hosted capacity
is unavailable.

Before enabling it, verify at least the following:

- Untrusted jobs have no path to private runners, workstation services, host
  mounts, credentials, or trusted caches. Disposable runners are destroyed
  after each job and do not pass writable state to release jobs.
- Pull-request jobs receive read-only repository tokens, cannot approve reviews,
  and receive no repository, environment, organization, signing, deployment, or
  identity-provider credentials. No job has unneeded token permissions.
- `pull_request_target`, `workflow_run`, issue/comment triggers, and reusable
  workflows cannot turn contributor-controlled code or artifacts into trusted
  execution. Do not use a privileged follow-up job to execute a pull-request
  artifact.
- Fork approval policy is explicitly checked for the new public repository;
  require approval for all outside contributors as an additional control, not
  as the runner isolation boundary. Pin third-party actions to reviewed full
  commit identifiers.
- A fork attempting to replace the workflow, select a private runner, request
  secrets or write permissions, and poison a later build cannot cross the
  boundary. Test this using synthetic material in the disposable environment,
  never a private workstation or real signing key.
- Signing and publication remain a separately authorized operation with their
  own protected credentials and review, not a consequence of a check passing.

Document what was actually tested and what could not be verified before
removing the workflow-free restriction. Private development checks can keep
running in the meantime; they do not certify public CI isolation.
