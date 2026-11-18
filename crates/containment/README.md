# Process containment

`Container::new` owns a per-run cgroup on Linux when available, a non-breakaway
Job Object on Windows, or an explicitly weaker process group otherwise. This
is process lifecycle containment, not filesystem or network isolation.

`Container::limited` requires qualified native aggregate resource enforcement.
Linux requires a delegated cgroup and never falls back to a process group.
**Resource-limited containers are not offered on Windows until native enforcement
is qualified.** The Windows constructor returns `HarnessError::Unsupported`
before creating any Job; it never substitutes partial limits or an unlimited Job.

Native Windows qualification found that the Job accepted and reported its CPU
hard-cap policy, but the two busy fixture processes used 5.23 CPU seconds against
a 0.86-second allowance. Console-host processes also increased the active Job
count beyond the requested process cap. The memory peak counter exceeded its
limit after an allocation refusal, so that counter alone does not establish the
amount of memory successfully committed. These failures remain visible in the
retained opt-in native tests; cross-compilation and policy readback are not proof
that the requested limits are enforced.

The internal Job-limit implementation remains available to those tests. Its
intended memory allowance covers aggregate private committed bytes, not physical
memory or shared mappings. Its CPU allowance is milliseconds of one logical CPU
per second for the whole tree, not a lifetime budget. The native CPU conversion
rounds down and refuses allowances below the smallest representable rate. None
of these implementation details makes the unqualified Windows limits available
to callers.

Unlimited Windows Jobs retain suspended-first launch, no-breakaway policy,
kill-on-last-handle-close and assignment-failure cleanup. Confirmation matches
process notifications against cumulative Job accounting and waits for every
pinned process handle to signal its exit; inactive accounting alone is not
enough. Missing notifications or inspection failures leave cleanup unconfirmed.
If an old process ID has been reused by an inaccessible unrelated process,
cleanup also stays unconfirmed rather than assuming that the original exited.
Job handles are not inherited. External
process adoption remains available for unlimited Jobs; the retained limited-Job
implementation refuses adoption because Windows does not charge allocations
made before assignment.

Trusted native launchers can borrow `Container::creation_job` for the
`PROC_THREAD_ATTRIBUTE_JOB_LIST` creation-time attribute. They must keep the
borrow alive through creation, verify membership before resuming, and terminate
the owned child on any failure. The Job handle must never enter the child's
inherited-handle list. Unsupported creation-time assignment must refuse rather
than falling back to post-creation assignment or a different Job.

## Tests

`cargo test --locked -p containment --lib` includes portable CPU conversion and
failure-policy tests. On Windows a normal unit regression verifies that the
public limited-container constructor refuses with an explicit unsupported reason;
it does not create or launch a Job. The existing synthetic Job lifecycle and
failed-assignment tests remain. None use a native agent or account.

Additional Windows tests are deliberately opt-in and operate only on authored
copies of the test executable, anonymous committed memory, temporary files,
and per-test Jobs. They make no profile, user, service, or file-permission changes.
They do not establish filesystem/network isolation. Their ignore reasons record
the unqualified CPU and process enforcement; these are outstanding qualification
tests, not passing release evidence. Run them serially on a suitable disposable
native test system, with a scratch build target:

```powershell
$env:CARGO_BUILD_JOBS = '2'
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP 'process-containment-tests'
cargo test --locked -p containment --lib job::limits_tests::limited_container_rejects_preallocated_external_process_without_assigning_it -- --ignored --exact --nocapture
cargo test --locked -p containment --lib job::limits_tests::limits_are_installed_before_launch_and_process_cap_refuses_descendants -- --ignored --exact --nocapture
cargo test --locked -p containment --lib job::limits_tests::committed_memory_cap_is_aggregate_and_recovers_after_cleanup -- --ignored --exact --nocapture
cargo test --locked -p containment --lib job::limits_tests::nested_job_cannot_escape_outer_limits_or_kill_on_close -- --ignored --exact --nocapture
cargo test --locked -p containment --lib job::limits_tests::cpu_rate_throttles_combined_descendants_not_lifetime_usage -- --ignored --exact --nocapture
```

The CPU test compares two continuously busy descendants with an uncapped
control. In each of three measurement windows it checks both processes remain
alive and publish increasing progress counters; it checks their statuses again
before cleanup. An early exit or a parked worker cannot count as throttling.
It fails rather than claiming throttling proof if competing machine load
prevents establishing the control. The memory test reaches the native
private-commit ceiling twice in fresh Jobs and checks cleanup/recovery. The
adoption regression owns a synthetic process that has already committed more
than the new limit, verifies limited adoption leaves that Job empty, and checks
unlimited adoption still works.
The adoption regression exercises the internal limited Job directly because the
public constructor now refuses. Restoring public Windows limited containers
requires resolving and qualifying native enforcement, not merely changing an
ignore reason or obtaining a successful policy-setting API result.
