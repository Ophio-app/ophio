# Self-hosted CI runner

This is the trusted, persistent pool. Use the [isolated runner
pool](isolated-runner.md) for `report/*` branches; it has a fresh runner and
home for each job and no `/ci-shared`. Never give a trusted runner the
`ophio-isolated` label.

The trusted persistent pool runs Linux build and separate dependency-advisory
checks on self-hosted runners. This avoids reliance on hosted runner availability;
it is not suitable for untrusted contributions. The second runner shares the
pool's read-only root and build cache. Keep the isolated pool separate.

## What runs where

| Item | Location |
|---|---|
| Runners | Two GitHub `actions/runner` 2.338.0 installs (SHA-256 checked against the release notes), in `~/.local/share/actions-runner/ophio` and `~/.local/share/actions-runner/ophio-2` |
| Root filesystem | Ubuntu 24.04.5 base (signature and checksum checked), provisioned by `scripts/ci-runner/provision-rootfs.sh` into `~/.local/share/actions-runner/ophio-rootfs`, shared read-only by both |
| Service | systemd user template `~/.config/systemd/user/ophio-actions-runner@.service`, one instance per runner folder (`@ophio`, `@ophio-2`), both in `ophio-ci.slice`. Copies are in `scripts/ci-runner/`. The user session lingers, so they start without a login |
| Label | Both runners carry one custom label and no default ones. The repository variable `CI_LINUX_RUNS_ON` names it |
| Jobs | The plan, Linux and advisory jobs run on `vars.CI_LINUX_RUNS_ON`, falling back to the built-in `self-hosted`, `Linux`, `X64` labels, which these runners don't carry |
| Windows job | Skipped while the repository variable `CI_WINDOWS_RUNS_ON` is `off` |

The Linux job's hosted path builds in an `ubuntu:24.04` container. The build host
runs Arch, whose glibc is newer than the 2.39 the Linux packages are built
against, and it has no Docker. Instead the runner itself runs in bubblewrap
on top of an Ubuntu 24.04 root that already holds the container's packages,
the runner's .NET dependencies and Playwright's Chromium libraries. On
the build host the workflow skips the container, disk clean-up and apt steps, and
installs only the locked Chromium.

The Rust browser tests look for `/usr/bin/chromium`. In the root that is a
small script which starts the Chromium path the job recorded in
`$HOME/.ophio-ci-chromium`, with its sandbox off, as the hosted wrapper does.

## Sandbox

The unit invokes the system `/usr/bin/bwrap` directly, and only then runs the
writable `run.sh`, so the runner's self-update and later listener launches
stay inside the same sandbox.

- `/` is the Ubuntu root, mounted read-only. Nothing of the host's `/home`,
  `/etc`, `/run` or `/tmp` is visible, apart from a read-only
  `/etc/resolv.conf`. Jobs cannot read `~/.config`, `~/Projects`, SSH keys or
  stored credentials, and the D-Bus, keyring and agent sockets are absent.
- Each runner's own folder is writable, at `/runner`, and so is the folder
  both share, `~/.local/share/actions-runner/ci-shared`, at `/ci-shared`.
  Nothing else on the host is. `HOME` is `/runner/home`, where npm,
  Playwright, git and the build caches keep their state. `/tmp`, `/run` and
  `/var/tmp` are private tmpfs mounts.
- Private user, PID, IPC and UTS namespaces; a minimal `/dev`; the
  environment is cleared. Jobs run as `runner` (uid 1000) and cannot install
  packages.
- `ophio-ci.slice` holds both runners: `CPUWeight=50` for the pair, memory
  throttled past 36 GB and capped at 40 GB together. Each runner also has
  `Nice=10` and its own 24 GB cap. Other work on the build host stays ahead
  of CI.

Checked on 2026-10-10 from inside the sandbox: credentials, `~/.ssh` and
`~/Projects` unreadable; `/usr` and `/etc` unwritable; remounting `/` refused;
host processes invisible; glibc 2.39.

Not isolated: the network, including services on `127.0.0.1` and abstract
Unix sockets. Nor is one job from the next: a job can change the runner's
files, caches and registration, and that persists. Cancelling a job signals
its step process, but a process that removes `RUNNER_TRACKING_ID` can outlive
it. Pull requests here come only from people with write access. Do not use
this runner for a public repository or outside pull requests.

Never run files from the runner folder outside bubblewrap, including
`config.sh`: jobs can rewrite them.

## What each push runs

The plan job (`scripts/ci_plan.py`) picks the `scripts/ci.sh` stages a
commit needs, and the Linux job runs only those; with none, it is skipped.
After the Linux job passes on this host, it records the commit's tree hash as
an empty file in `/ci-shared/passed`, which both runners read.

- A tree that already passed runs nothing. A pull request's run records its
  merge commit's tree, so the squash merge to main with the same tree is not
  built again.
- Otherwise the plan compares the commit with the nearest of its last 50
  first-parent ancestors whose tree passed. A change only to the phone app's
  screens runs the client and Tauri stages; a change only to Rust the app
  doesn't read skips the client stage. Files one half reads from the other,
  the crates that export protocol bindings, the bindings themselves and the
  client's lock files run both. Docs run only the quick whole-tree stages.
  Anything else, such as the scripts or the workflow, runs every stage.
- Two stages take most of a full run, so a change runs them only for the
  files they check. The repeated test runs (`parallel`: the whole suite
  three times, then twice with every core busy, about 15 minutes) run when
  `crates/test-support` changes. The release packages (`package-linux` and
  `package-windows`, about 5 minutes) are built when `packaging/`, the
  `scripts/package*` files or a Cargo manifest or lock file changes. A Rust
  change otherwise takes about 7 minutes, a screens-only change about a
  minute. Every update build still packages, and a run started by hand runs
  every stage. The 25-and-10 repeat run for qualification is separate
  (`docs/qualification/linux.md`).
- With no passed ancestor in reach, or when started by hand from the Actions
  tab, every stage runs. GitHub's hosted runners keep no record, so there
  every stage always runs too.
- The advisory job runs on every push whatever changed, since a new advisory
  can fail an unchanged tree.

Marks older than 90 days are deleted when a new one is recorded. Jobs can
write `/ci-shared`, so a job could mark a tree it never passed; that is
within the trust boundary below, where anyone who can start a job can already
change the runner itself. These marks are not evidence for untrusted code;
report branches skip both reading and writing them and run all stages on
the isolated pool. To make the next push run everything, empty the
folder:

```sh
rm -f ~/.local/share/actions-runner/ci-shared/passed/*
```

## Build caches

On this host the Linux and advisory jobs keep Rust's toolchains, the
registry, the cross compiler and the build folder in each runner's
`$HOME/ci-cache` (`scripts/ci-folders.sh`), and npm keeps its own cache in
that home. Nothing is restored from GitHub's cache. A build folder that has
grown past 100 GiB (`CI_TARGET_CAP_GIB`) is emptied at the start of the next
job. To reclaim the space by hand, with no job running:

```sh
find ~/.local/share/actions-runner -maxdepth 4 -path '*/ci-cache/target' -prune -exec rm -rf {} +
```

On GitHub's runners the same jobs build inside the checkout under `.ci-tools`
and `target`, restored by `actions/cache`.

## Operating it

```sh
systemctl --user status 'ophio-actions-runner@*'
journalctl --user -u ophio-actions-runner@ophio -f
systemctl --user restart ophio-actions-runner@ophio-2   # only when it runs no job
systemctl --user disable --now ophio-actions-runner@ophio ophio-actions-runner@ophio-2   # stop CI here
```

After editing a unit or the slice in `scripts/ci-runner/`, copy it to
`~/.config/systemd/user/` and run `systemctl --user daemon-reload`, then
restart each runner while it is idle.

The unit uses `Restart=always`, because `run.sh` reports several listener
failures as exit 0. After a restart GitHub can refuse the runner for about two
minutes ("A session for this runner already exists"). A job queued in that
window can stay queued once the runner is back; cancel and re-run it. GitHub
fails jobs that wait more than 24 hours for a runner.

### Adding a runner

A runner folder must be registered from inside the sandbox, since jobs can
rewrite `config.sh`. With `N` the new instance name:

1. Make the folder and its home, and unpack the checked runner tarball into
   it: `mkdir -p ~/.local/share/actions-runner/N/home` then
   `tar -xzf actions-runner-linux-x64-2.338.0.tar.gz -C ~/.local/share/actions-runner/N`.
2. Get a registration token from Settings › Actions › Runners › New
   self-hosted runner, or `POST /repos/{owner}/{repo}/actions/runners/registration-token`.
3. Pipe the token into `systemd-run --user --wait --pipe` running the same
   bubblewrap command line as the unit's `ExecStart`, with the new folder
   bound at `/runner`, and in place of `run.sh` start
   `/bin/sh -c 'read -r ACTIONS_RUNNER_INPUT_TOKEN && export ACTIONS_RUNNER_INPUT_TOKEN && exec /runner/config.sh --unattended --url https://github.com/OWNER/REPO --name NAME --labels LABEL --no-default-labels --work _work'`.
   The token never appears on a command line.
4. `systemctl --user enable --now ophio-actions-runner@N`.

### Changing the root

To add packages, edit `scripts/ci-runner/provision-rootfs.sh` and re-run it
with the runner stopped. It reuses an existing root and needs no root
privileges, but it must be started from a session that may create user
namespaces. From a trusted checkout accessible to the user service manager
(not an agent's private temporary directory), run the following. `--same-dir`
keeps the script and tarball paths relative to that checkout rather than the
service manager's default home directory:

```sh
systemd-run --user --wait --pipe --same-dir unshare --map-auto --map-root-user --mount \
  sh scripts/ci-runner/provision-rootfs.sh \
  ~/.local/share/actions-runner/ophio-rootfs ubuntu-base-24.04.5-base-amd64.tar.gz
```

## Windows

The build host cannot run the native Windows job. While `CI_WINDOWS_RUNS_ON` is
`off` it is skipped, and the Windows cross-compile and packaging checks in
`scripts/ci.sh` still run in the Linux job. Delete the variable to use
`windows-2025` when hosted jobs are available, or set it to the label of a
self-hosted Windows runner.

## Going back to GitHub-hosted runners

Set the repository variable `CI_LINUX_RUNS_ON` to `ubuntu-24.04` and delete
`CI_WINDOWS_RUNS_ON` (Settings › Secrets and variables › Actions ›
Variables). The Linux job then uses the container path again. To remove the
local runner, stop and disable the service, delete its registration in
Settings › Actions › Runners, then delete the runner and root folders and the
unit file and run `systemctl --user daemon-reload`.

The advisory job uses the same runner selection as the Linux build, retaining
its independent result. See [Build caches](#build-caches) for where each kind
of runner keeps its tools.
