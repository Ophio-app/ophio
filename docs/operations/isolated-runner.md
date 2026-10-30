# Isolated CI runners

Branches named `report/*` can contain code derived from untrusted reports.
Their build scripts and tests must be treated as arbitrary code. The
[trusted runners](self-hosted-runner.md) retain their runner installations,
registration, homes and caches between jobs. They also share writable
`/ci-shared/passed` marks. A job there could plant a file for the next job,
copy reusable registration credentials, or forge a passed tree.

The isolated pool uses only `self-hosted` and `ophio-isolated`. Never add
`ophio-isolated` to a trusted runner, or the trusted custom label, `Linux`
or `X64` to an isolated runner. The supervisor refuses an API response with
any other labels. Labels select runners; they are not an authorization
boundary against someone who can change a workflow.

## One job per sandbox

`scripts/ci-runner/isolated-job.sh` runs on the host from an installed,
trusted copy, never from a checkout or a runner folder. Each invocation:

1. Makes a private, uniquely named attempt folder and copies an unused,
   read-only runner installation with `cp --reflink=auto`. It makes that
   copy writable and creates its empty home.
2. Reads a private host token file and requests a repository JIT config at
   `POST /repos/OWNER/REPO/actions/runners/generate-jitconfig`. Only the
   one-job JIT credential enters the sandbox. The API token never does;
   it is absent from command arguments, the runner environment and mounts.
   Redirects and host proxy environment variables are ignored; API error
   bodies and configs are never printed by the supervisor.
3. Starts the copy's `run.sh --jitconfig` inside bubblewrap. The Ubuntu
   filesystem is read-only, and only that attempt's `/runner` is bound
   writable. Home and all build caches live in that copy; `/tmp`, `/run`
   and `/var/tmp` are private tmpfs mounts. The root's top-level entries
   are mounted separately, excluding the trusted `/ci-shared` mount point,
   and the new root is remounted read-only. There is no `/ci-shared`, even
   when the shared Ubuntu root contains that empty directory.
4. Clears the environment, uses private user, PID, IPC and UTS namespaces,
   `--die-with-parent` and `--new-session`, and waits at most
   `ISOLATED_TIMEOUT_SECONDS` (default 7200, including idle queue time).
   GNU `timeout` sends TERM, then KILL after ten seconds. Destroying the
   PID namespace ends descendants, including processes that detach or
   remove runner tracking variables.
5. Deletes the entire attempt on success, failure, timeout or handled
   signal, restoring directory permissions first if necessary. It appends
   one line to the host's `jobs.log`: `job_id`, supervisor `exit` and
   `duration_seconds`. The id is a local attempt id, also used in the JIT
   runner name; it is not the GitHub workflow job's numeric id. The exit
   is the runner process status, not proof that workflow checks passed.

The user unit loops invocations with a 15-second delay, including after
failures. Each instance has `MemoryMax=24G` and `Nice=10`, and belongs to
the existing `ophio-ci.slice` (36 GB memory high, 40 GB maximum and CPU
weight 50 across both pools). Starting more instances reduces the memory
available to the trusted runners. Keep the initial pool to one instance.

This removes writable state shared with other jobs. It is still a Linux
namespace sandbox sharing the host kernel and network, including localhost
services and abstract Unix sockets. Audit those services before accepting
untrusted work. A VM or separate host provides a stronger boundary. Do not
add host homes, sockets, credentials, shared caches or signing access.

## Routing assumptions

`checks.yml` tests `github.head_ref || github.ref_name`, so both pushes to
`report/*` and pull requests whose source branch is `report/*` use the
isolated pool for plan, Linux and advisory jobs. This takes precedence over
`CI_LINUX_RUNS_ON`, including when it selects a hosted container. Report
plans never read passed-tree marks, and the record step is skipped. Without
marks, every stage runs. Native Windows jobs are skipped for these heads;
the Linux job still runs the Windows cross checks and unsigned packaging
checks. Native coverage requires a separate ephemeral Windows pool.
Other branches retain the variable overrides and existing runner defaults.

GitHub reads the pushed branch's workflow for a `push` event. These routing
rules therefore hold only if the report publisher enforces a protected
baseline containing this workflow and refuses workflow changes before
every push. It must also reject changes to CI scripts, runner scripts,
build scripts and other sensitive paths, require the `report/` prefix,
and prevent report code from being pushed under trusted branch names.
Old report branches must be recreated or brought onto this baseline before
they can run; blocking workflow diffs alone does not repair an old workflow.
Apply the same gate to PR source branches and their merge result. This
pool does not implement the publisher's gate. If those guarantees cannot
be enforced, keep report publication disabled and restrict the trusted
runner group to approved workflows where the account supports it. A
changed workflow can request the trusted label directly.

## Install (operator only)

These steps change the live host. Review and apply them separately from
development; do not restart or modify the trusted runner instances.

1. Confirm bubblewrap namespaces work in the user service context, and
   that GNU `timeout`, GNU `cp`, Python 3 and the existing slice are present.
   Reuse the provisioned Ubuntu root from `provision-rootfs.sh`, read-only.
   If it does not exist, follow the root provisioning instructions in
   [the trusted runner guide](self-hosted-runner.md#changing-the-root).
   Never reprovision a root while either pool uses it.
2. Download a current, pinned Linux x64 `actions/runner` release and verify
   its SHA-256 against the official release. Extract into a **new**
   `~/.local/share/actions-runner/isolated-pristine` directory. Never copy
   an existing registered runner. Do not run `config.sh` or `run.sh` there.
   Remove no registration files to make an old install look pristine;
   extract a new archive instead. Make the entire install read-only with
   `chmod -R a-w`. The supervisor rejects writable installs or ones with
   registration, environment, work or home state. Replace the pristine
   install from another verified release when upgrading; never promote a
   job's updated copy into it.
3. Create a token dedicated to this repository and store it in a host-only
   file owned by the supervisor's user, mode `0600`, with its parent mode
   `0700`. Use a fine-grained PAT restricted to this repository with
   **Administration: write**, or a GitHub App installation token with that
   permission. This is a powerful permission: keep its lifetime short and
   arrange host-side rotation (App tokens expire); it is not an ordinary
   runner registration token. No token refresh is done in the sandbox.
   See GitHub's [repository JIT endpoint](https://docs.github.com/en/rest/actions/self-hosted-runners#create-configuration-for-a-just-in-time-runner-for-a-repository)
   for the permission and request contract. Do not put this token in
   workflow secrets, the Ubuntu root, the pristine install or a job folder.
4. Install the reviewed supervisor outside all job mounts, then create a
   private environment file for the first instance. Paths must be absolute;
   the root, pristine install and state folder must be separate. The token
   file must be outside the mounted root and pristine install. For example,
   `~/.config/ophio-ci/isolated-one.env` contains the following, replacing
   `/home/you` and `OWNER/REPO`:

   ```ini
   ISOLATED_REPOSITORY=OWNER/REPO
   ISOLATED_TOKEN_FILE=/home/you/.config/ophio-ci/runner-token
   ISOLATED_RUNNER_INSTALL=/home/you/.local/share/actions-runner/isolated-pristine
   ISOLATED_ROOTFS=/home/you/.local/share/actions-runner/ophio-rootfs
   ISOLATED_STATE_DIR=/home/you/.local/share/actions-runner/isolated-one
   ISOLATED_RUNNER_GROUP_ID=1
   ISOLATED_TIMEOUT_SECONDS=7200
   ```

   Use the actual repository runner group id if it differs. The state
   folder is created mode `0700`; give each instance its own state folder.
   The default API URL is `https://api.github.com`. `ISOLATED_API_URL` allows
   an HTTPS enterprise API base or a loopback HTTP fake for testing.
   `ISOLATED_BWRAP` is a test injection point: leave it unset in production.

5. From a trusted checkout, after replacing `TRUSTED_CHECKOUT` below:

   ```sh
   install -Dm755 TRUSTED_CHECKOUT/scripts/ci-runner/isolated-job.sh \
     ~/.local/libexec/ophio-ci/isolated-job.sh
   install -Dm644 TRUSTED_CHECKOUT/scripts/ci-runner/ophio-isolated-runner@.service \
     ~/.config/systemd/user/ophio-isolated-runner@.service
   systemd-analyze --user verify ~/.config/systemd/user/ophio-isolated-runner@.service
   systemctl --user daemon-reload
   systemctl --user enable --now ophio-isolated-runner@one
   ```

   No persistent runner registration step is needed. Keep report publishing
   disabled until routing, labels and the publisher gates have been verified.

## Verify isolation

Run the local tests without installing or starting a unit:

```sh
python3 -m unittest discover -s scripts/ci-runner -p 'test_*.py' -v
bash -n scripts/ci-runner/isolated-job.sh
```

They use a loopback fake API and an unused fake runner, check exact labels,
distinct copies, clean homes, environment clearing, mounts, redacted errors,
failure and signal cleanup, and a timeout that kills descendants. The
injected bwrap substitute validates command construction and runs the fake
runner; it provides **no filesystem isolation**. If namespace creation is
available, an additional test runs the real command against a minimal root
with a planted shared mark and probes token and shared-folder invisibility,
read-only root, repeated fresh jobs and timeout cleanup. A skip means these
kernel and mount properties still need verification on the deployment host.

After installation, use an operator-controlled report branch based on the
approved workflow. Confirm plan, Linux and advisories select only the two
isolated labels, even with a trusted `CI_LINUX_RUNS_ON` override. From a
smoke job check:

```sh
test ! -e /ci-shared
test "$HOME" = /runner/home
test ! -w /usr
test ! -e /home/you/.config/ophio-ci/runner-token
test -z "${ISOLATED_TOKEN_FILE:-}"
```

Confirm another job starts with no planted file or home cache, and the
previous attempt folder has disappeared. Check the host's `jobs.log`,
`journalctl --user -u ophio-isolated-runner@one`, and the instance's slice
and memory properties. Test a deliberately short wall limit in a separate
instance with the fake API first. Keep workflow success checks authoritative;
the runner wrapper can mask a listener failure as exit zero.

The summary log is host-owned and never mounted into jobs; rotate it on
the host. Runner diagnostics inside the job folder are deliberately deleted.
SIGKILL of the supervisor, power loss or a filesystem failure cannot run a
shell trap. With the isolated unit stopped and no isolated jobs running,
inspect and remove any stale `ISOLATED_STATE_DIR/jobs/job.*` folders; never
reuse them. Namespace destruction ends live descendants, but deletion is
not secure erasure of storage snapshots or blocks.

## Remove

Disable report publication first so jobs do not silently fall back to other
pools. Stop and disable only `ophio-isolated-runner@one` (and any other
isolated instances). A normal stop signals the supervisor and children,
allowing cleanup; verify the unit is inactive and its job folders empty.
Revoke the dedicated token and remove stale isolated runner entries through
GitHub's runner settings if a failed start left any. Remove only the
isolated unit, installed supervisor, its environment and token files, the
isolated state folders and pristine install, then daemon-reload. Retain or
archive the summary log as needed. Leave `ophio-ci.slice`, the Ubuntu root,
trusted runner registrations and trusted runner folders in place.
