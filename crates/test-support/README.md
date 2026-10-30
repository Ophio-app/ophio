# Test process fixtures

Use `test_support::executable(path, bytes)` for every Unix fake executable.
Add this crate as a **dev dependency** only. The helper requires `install`
(coreutils on Linux); Python harness fixtures also require `python3`.

Writing or copying an executable in the multithreaded test process is unsafe
for parallel spawning even if the file is closed before executing it. A sibling
fork inherits the open writer until exec, allowing `ETXTBSY` from the kernel.
Renaming the written inode or setting close-on-exec does not prevent this.
The helper writes a separate source file and has a child process create the
executable. Only that child can hold its write descriptor, and the helper waits
for the child to exit. Do not bypass this helper with writes, copies, or chmod
on executable files. Directory permissions and non-executable data files are
unaffected.

Instance locks have a related lifetime hazard. `store::InstanceLock` explicitly
unlocks in its destructor; merely closing the owning file can leave its lock
held by an inherited descriptor. Keep the guard alive until protected work is
drained. Its regression test holds a duplicate descriptor open across release,
which shares the same Unix open-file description as an inherited descriptor.
No test-global mutex, serialized runner, sleep, or spawn retry is required.

For non-executable readiness files, existence must mean the contents are
complete. The force-stop fixture writes its child PID to a sibling temporary
file, closes it, then renames it to `child.pid`. Its socket-gated regressions
hold the writer between create and write, check that `child.pid` is still
absent, and then verify the published PID and process cleanup for both harnesses.

The Linux services force-stop fixtures bind Unix socket gates through
`/proc/self/fd/<directory-fd>/<name>` so long `TMPDIR` values cannot exhaust
`sockaddr_un`. The socket files, project, executable, and hub data remain inside
an owned temporary directory under `TMPDIR`; native fixtures connect by relative
name from their project directory. Bind gates before starting native processes.
If later setup panics, shut down the hub while its runtime is still alive, then
propagate the panic and let the temporary directory remove the artifacts.
Regressions exercise both harnesses under a path longer than 108 bytes and
inject a setup failure with a live parent and child.

For repeatability verification, select the workspace toolchain, then run:

```sh
export CARGO_TARGET_DIR="$PWD/target" CARGO_BUILD_JOBS=8
bash crates/test-support/check-parallel.sh
```

This runs 25 complete workspace suites followed by 10 with `nproc` CPU hogs.
Every run uses Cargo's default test parallelism and removes desktop display and
session bus variables. It stops on failure, reports each run's test counts,
retains logs under `target/parallel-evidence/acceptance`, and kills its own hogs
on exit. It needs Bash, Python 3, `nproc`, and `yes`.

Inbox fixtures compare stored paths with the canonical temporary parent, while
passing the original path to production. This also works when `TMPDIR` names
an inherited directory handle such as `/proc/self/fd/9`.
