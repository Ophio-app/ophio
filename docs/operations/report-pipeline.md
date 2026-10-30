# Report fix checks

`report-pipeline` is a library. It runs no agents, binds no sockets and has no
GitHub client. Assume every model reading a report is hijacked: a report may
speed up work, but cannot authorize a reply, a merge, signing or a release.

Nothing starts by itself: the intake gives the worker only reports the owner
chose with Look into it in their report inbox (see `intake.md`). The one
thing the worker does unasked is check a test submission when it arrives:
it runs the reporter's test, contained, and records what it showed for the
owner. No model runs in a check.

The caller owns this sequence:

1. Check the pause switch and reserve a budget ticket before each stage.
2. Render one bundle for sandboxed triage, with indexed documentation and
   duplicate candidates. Parse its bounded JSON with `parse_triage`.
3. Validate the new repro file against the base tree and freeze its SHA-256.
   Prove it compiles and fails at base in an isolated sandbox.
4. Give the fix agent only `render::fix(verdict, reduced_test)`. Its type
   accepts no bundle, trail, crash message or reporter words.
5. Inspect full commit hashes with `CommitDiff::inspect`, then `evaluate`.
   Bind test evidence to those exact commits and the frozen test hash.
6. Render blind review with only the verified diff, its repro reduced, and
   gate flags. A block or malformed review stops; a pass cannot override a
   gate.
7. Run isolated CI. Show the owner the test, diff, gate results and risks.
   The owner merges. Sign provenance after approval; signing the application
   is a separate authority. Offer release fixes by signature, widening from
   owner devices to early users to everyone.

Every library error stops the caller. A `Fail` stops the run. An `Owner` may
continue to isolated CI, but **requires the owner's own review** and must be
prominent in approval and provenance. Even `Pass` still needs owner approval.
Do not expose the pipeline database, key, git object store or base checkout to
agents. Import completed commits into a trusted repository before inspection.
Do not execute builds or git hooks in the process holding the pipeline key.

## Schema and test boundary

Agent JSON has a 4 KiB cap, closed enums and unknown-field rejection, including
nested questions. Candidate/doc indices are range checked against trusted
lists. All agent strings are ASCII relative test paths of at most 240 bytes.
Questions convert to contract templates; the agent cannot supply wording or a
build string. A bug stays `Received` until code proves reproduction. Duplicates
become `Known`, setup problems require a template question, and the remaining
categories map to the contract's decline reasons or a template question.
A bug that matches the documentation is rejected for reclassification.

The test-path, size and character rules live in `protocol::reports`
(`valid_test_path`, `valid_test_source`, `safe_code_text`) and are the same
for an agent's repro and a reporter's submitted test; the intake applies them
on arrival. The tree-dependent checks stay here, in `ReproTest::validate`.

Repros must be new ordinary files, in an existing base-tree directory:
`crates/<crate>/tests/**/*.rs`, `crates/<crate>/src/**/*_tests.rs`, or
`client/src/**/*.test.ts` / `*.test.tsx`. Unit-test registration and vitest
selection belong to the sandbox runner; a file merely present on disk is not
proof it ran. Keep the raw test outside the fix sandbox: the fix agent can read
only the reduced file. After it exits, the trusted caller injects the frozen raw
test into the final commit. The repro checker and final isolated CI execute the
identical raw test at base and fix.
File rejection carries an off-schema hijack signal. Files are UTF-8, at most
8 KiB, without NUL or bidi controls. The final commit
must add the exact frozen bytes. Editing or omitting the repro is tampering.

Reduction removes Rust/TS comments while preserving whitespace and strings,
including Rust raw strings, nested Rust comments, and whole TS templates.
Removed spans are recorded as offsets, byte counts and hashes, never text.
Literals over 256 bytes block fix rendering. Ambiguous TS slash syntax
(regex/division) is rejected instead of guessed. This is a small lexer, not a
language parser or a security proof: short literals, identifiers, templates,
macros and executable test code can still carry instructions or malicious code.
Tests and fixes must always run without credentials and behind containment.

## Test submissions

A reporter's own model can send a failing test against the source of their
build instead of words; see `intake.md`. Every part of it is a claim, and the
test is hostile code until a contained run says otherwise.

**The check.** Whenever the worker runs and isn't paused, it checks one
test submission per round, apart from the single active report slot, so a
report waiting on CI or the owner doesn't hold checks up and checks don't
take the slot. It reads the day's check budget first and claims nothing when
it is spent. Then it claims the oldest unchecked submission
(`ClaimCheck`), fetches, and asks git whether the reporter's commit is in
the mirror and part of main. Only git's plain "no" refuses the build side
(`unknown_commit` or `not_on_main`); any other git failure is ours. At their
commit it refuses a path already taken, a folder that doesn't exist and a
crate without its manifest (or library, for a unit test), then runs only the
submitted test, raw, with the same contained, offline, credential-free
runner the Repro stage uses and the same tamper check on commit and test
hash. No agent sees a check, so the comment remover plays no part in it.
Then it does the same at main's tip.

A Rust test is built with `--no-run` first, and only then run, so a build
that fails reads `does_not_build` and a run that dies never does. Each side
records `fails` (the run reported an assertion failure in libtest's own
shape, a `panicked at` line directly followed by `assertion failed:` or
``assertion `left == right` failed``, or a Vitest failure whose message
starts with `AssertionError:`), `fails_without_assertion` (a reported
failure without that shape), `passes`, `does_not_build`, `no_test_ran`,
`timed_out`, `over_limits` (killed for memory, CPU time, file size or disk,
or more output than the runner keeps), `crashed` (built, then stopped before
a result: an abort, stack overflow or early exit) or `unreadable_output`
(Vitest printed no report the runner could read). The test controls its own
output and exit, so `fails` is what the run reported, not proof of a bug, and
the gate's red-to-green evidence carries the same caveat. When a run fails,
the runner reads its own output, with `RUST_BACKTRACE=1`, for the first
`path:line` in our tree that names a real file other than the test; the
worker checks it against the tree at that commit and records it as the
failure site. The test can print anything, so a site is a hint: it is always
a real file, but not always the right one. The check also records
`agent_block` when no agent may be shown the test even reduced: code the
small lexer can't read safely (JSX and regular expressions in most Vitest
files) or a literal over 256 bytes. Look into it can't use such a test.

Our own trouble never reads as the submission's, and the submission's own
doing is never retried. A failed fetch, a git failure or an unavailable
runner puts the check off with the recovery settings' backoff
(`DeferCheck`). Retries stop after `retry_limit` tries or once they have
redone a whole check's wall time of work, whichever comes first, and then
record `not_run` with `runner_unavailable`. Offline caches that lack the
dependencies an older commit locks record `not_run` with
`missing_dependencies`, never `does_not_build`. A canary, blocked access or
tampered evidence records `refused` with `hostile_signal` and counts one
strike against the reporter for that submission, however often recording it
is retried; three strikes ban them from checks and fixes. A test that breaks
the shared rules after the intake accepted it counts the same way.

`[pipeline.budgets]` adds `daily_checks` (24), `daily_reporter_checks` (3)
and `check_wall_ms` (25 minutes, for both runs together). A check is charged
once per submission before it runs; retrying after our own trouble costs
nothing more. A spent day leaves submissions unchecked until the next UTC
day, without a strike. Keys are free to make, so the overall daily cap is
the real bound. The worker never reads a submission's suggested fix: it is
only carried, for the owner.

**Look into it.** When the owner chooses a test submission, the worker skips
model triage. Trusted code builds the verdict: a bug, in the area its path
names (the crate, or the client screen), reproduced by the submitted test.
A test no agent may be shown, even reduced, stops there as
`UnshowableTest`, without a strike and without a word to the reporter. A test
that can't go into main as sent (its path is taken or its folder gone there)
stops as not reproduced, without a strike. Otherwise the test becomes the
frozen repro at main's tip and everything after is as for any report.

**What agents never see.** No agent sees a submitted test's raw text, its
comments or its suggested fix, or the reporter's model name. The fix agent
gets only `render::fix(verdict, reduced_test)`, and the reviewer's diff shows
the repro reduced too (`CommitDiff::with_reduced_test`); gates still judge
the real diff, and the final commit carries the exact raw test. The
reviewer's diff takes the repro's file only at its own header, which must
appear once at a line start. Rendering a test submission for triage is
refused outright. Submitted code, like any repro, can carry no NUL,
direction controls or other invisible format characters.

## Gates

Red-to-green requires an assertion failure (not a compile error, skip or
timeout) at base and a pass at fix. The runner reads an assertion failure
only in the harness's own shape (libtest's assertion panic, Vitest's
`AssertionError`), but a test writes its own output, so this is what the run
reported, not proof: the owner still reads the test. Other supplied test results cannot worsen,
disappear or become unproven. The orchestrator supplies the complete test
inventory; this library cannot discover omitted test execution.

Dependency-table changes in Cargo manifests (including target/workspace,
dev/build tables, patch/replace) fail. npm dependency changes, install scripts,
overrides and workspace resolution changes fail. Workflow changes under
`.github/workflows/` fail before branch publication, so a proposed workflow
cannot select a different runner or request a publishing token before review.
Lockfile changes, new `build.rs`, `.cargo/config*`, `.npmrc`, submodules and
vendored code fail.
Malformed manifests stop inspection. Formatting-only Cargo changes are allowed
through this gate, but still need owner review.

The default cap is 8 changed files and 300 added plus removed lines. Binary
files, non-UTF-8 blobs, symlinks, mode changes and deleted tests fail. Existing
test edits and source edits in files containing unit tests require owner
review, conservatively including changes that may leave assertions intact.
Diffs use commit objects, no rename folding, no external diff or text conversion,
and force text so attributes cannot conceal added lines. A 1 MiB blob limit,
8 MiB aggregate blob/output limit, 4 MiB patch limit and inventory cap bound inspection; exceeding one stops it.

Sensitive paths always require owner review. Matching is case insensitive;
`*` spans directories. This list is checked against real repository files and
kept identical to `SENSITIVE_GLOBS` by a test:

<!-- sensitive-globs:start -->
- `crates/companion/*`
- `crates/services/*`
- `crates/store/*`
- `crates/domain/*`
- `crates/protocol/*`
- `crates/accounts/*`
- `crates/sandbox/*`
- `crates/containment/*`
- `crates/harness-*`
- `crates/platform-*`
- `crates/browser/*`
- `crates/report-*`
- `client/src-tauri/*`
- `client/src/host/*`
- `client/src/protocol/*`
- `client/src/platform/*`
- `client/src/screens/welcome/*`
- `*approv*`
- `*permission*`
- `*access*`
- `*grant*`
- `*pair*`
- `*identity*`
- `*keys*`
- `*update*`
- `*signature*`
- `*auth*`
- `.github/*`
- `.git*`
- `client/scripts/*`
- `scripts/*`
- `packaging/*`
- `.cargo/*`
- `*cargo.toml`
- `*package.json`
- `*lock`
- `*lock.json`
- `*build.rs`
- `*androidmanifest.xml`
- `*tauri.conf*`
- `*capabilities/*`
<!-- sensitive-globs:end -->

Future-path sentinels also protect any path containing `relay`, `intake`,
`crypto`, `credential`, `signing`, `person_only`, `endpoint` or `network`, and
any `Info.plist`. Some of these have no current file, so they are checked by
synthetic path cases rather than the existing-file glob check. Broad crate
rules cover pairing, person-only storage, approvals, permissions, containment,
identity, keys, endpoints, build tooling, packaging and update paths.

Added URL/host indicators, `unsafe`, process spawning, environment reads and
home-directory paths also require owner review. These are conservative lexical
indicators, not complete semantic analysis: aliases, computation and encoding
can evade them. Sensitive paths, containment, review and the owner's decision
remain necessary.

## Budgets and pause

`Budget::open` uses bundled SQLite in WAL mode. Defaults: 6 model attempts per
run, 30 minutes, 80,000 input and 20,000 output tokens; per model stage, reserve
12,000 input and 3,000 output tokens. Daily UTC caps are 200,000 total tokens,
6 CI runs, 1 release and 2 new pipeline runs per reporter key, and separately
24 test submission checks, 3 per reporter key. `BudgetConfig`
sets every limit. Accounting uses integer counts and rejects overflow.

`check(stage, context, pause_source)` atomically reserves quota and returns
`Allowed(ticket)` or `Stop(reason)`. The relay must enforce the ticket's input,
output and wall deadline continuously, and terminate sandbox work at the
deadline. Tickets expire at UTC midnight so one stage cannot spend yesterday's
reservation today. `record(ticket, usage)` settles actual input/output usage
and refunds the unused reservation. A lost ticket stays fully charged across
restart. An overrun is charged and locally pauses the pipeline. Never retry an
unknown completion with a refunded ticket. Attempts, CI and release counts
are charged before work, even if it fails. The orchestrator supplies trusted
UTC time, usage numbers and reporter identity; agent claims are never inputs.

`PauseSource` carries the intake switch. A failed switch read stops. The
persistent local pause is an additional stop; clearing it never overrides the
intake's pause. Bans persist with a bounded administrative reason. Budget
exhaustion stops without blaming the reporter. The orchestrator records and
applies hostile-signal strikes and chooses any ban policy.

## Signals, canaries and oscillation

Off-schema output, blocked sandbox paths/network, active canaries and gate
tampering stop, quarantine the report and count against the reporter. Relay
refusal and budget exhaustion stop without a hostile strike.

`Honeytokens::generate` makes random, clearly generated invalid provider,
GitHub, keystore and SSH-shaped decoys. `plant` exclusively creates private
files beneath a fresh canonical sandbox staging root (`home/` and `clone/`),
refusing symlinks and overwrites. Planting assumes exclusive ownership of that
root; never race an agent against filesystem preparation. A planting error
may leave partial decoys: discard the whole staging root.

Discard sandboxes after a caller restart; active canaries are held in memory.
The scanner is linear, allocation-free and matches each run's unique marker.
Scan agent output, tests, patches, relay bodies and logs before using them.
Scan decoded bodies and reassembled streams too: chunk splitting, encoding,
transformations or partial leaks can evade raw-byte matching. It is an alarm,
not an exfiltration proof. A sandbox must observe blocked accesses; merely
reading a decoy without emitting it cannot be observed by this library.

`Oscillation::remember` stores merged report references, signature digests,
areas, times and normalized added/removed line hashes. It stores no line text.
Within 30 days, removing a previous fix's added line or re-adding its removed
line stops for the owner, naming both reports. Different signatures in the
same area flag a possible conflict. Structural-only lines are ignored to
reduce noise; line-based matching can still flag shared code or miss semantic
reversals. Keep the history outside the sandboxes.

## Provenance and corpus

`Statement::new` chains SHA-256 links for the received bundle, checked triage,
frozen test, base commit, diff, gates, review, CI, approval and release build.
Only hashes and bounded identifiers are serialized, never bundle words,
trails, screenshots or test/diff bodies. `PipelineKey` creates the dedicated
`report-pipeline.ed25519` file at mode `0600`, rejecting symlinks, hardlinks,
wrong size and loose permissions. Key-file storage currently requires Unix;
Windows callers must supply a future protected key-store adapter.
This is never the application's release key. Keep it outside all agent roots.
`verify` checks the ordered chain, metadata and Ed25519 signature against a
separately pinned public key. `Statement::matches` checks supplied artifact
bytes against a link; a signature alone cannot prove a sandbox ran honestly.
The orchestrator keeps the original received bundle hash before redaction or
rendering and binds CI/approval/release ids to trusted results.

The JSON files in `crates/report-pipeline/corpus/` contain 88 contract-valid attacks
in words, trail/error/crash fields, version fields, context, state and extras.
`corpus::load` checks contract caps. To add a case, copy a bundle, inject the
payload within its field's cap, choose a scripted `Attack`, and name its
expected `Layer`. `HijackedAgentFake` deliberately emits hostile output or
patches without obeying the rendered prompt. The corpus integration test runs
all cases through actual parsers, frozen test checks, real temporary git
commits, gates, budgets and canary scans as applicable. `CorpusRunner` and
`corpus::run` are the seam for a later real sandbox/relay runner. The library
suite separately checks persistence, oscillation, prompt taint and provenance.
Run `cargo test -p report-pipeline`; tests require git and no network.

The owner still judges intended behavior, subtle security changes, unrelated
or misleading tests, false positives, conflicts, release timing and rollout.
Real sandbox containment, relay enforcement, CI isolation, the approval UI,
release feed and recovery remain responsibilities of the caller.

## Running the worker

The [security limits](report-pipeline-security.md) list remaining activation
blockers. Keep the worker disabled until those are addressed.

`report-fixer` supplies that caller. The companion starts one background worker
only when `[pipeline]` exists and `enabled = true`. A missing credential, CLI,
shim, cache or connected GitHub project leaves it off, with one reason in the
startup log. Restart the companion after changing the section. No worker uses
the person's working copy: `mirror` must be a new path owned by the pipeline.
An existing directory without the worker's ownership marker is refused.

Install the isolated ephemeral runner pool first; see
[isolated runners](self-hosted-runner.md). Report branches must run the `Checks`
workflow on runners labelled `ophio-isolated`. The worker checks the exact
commit, workflow result, job results and runner labels before asking the owner,
and the Merge action checks them again. Missing checks keep the report waiting;
failed checks stop it. Nothing falls back to the ordinary runner pool.

For example, in the companion's TOML configuration:

```toml
[pipeline]
enabled = true
intake_url = "https://private-intake.example.invalid/"
intake_token_file = "/absolute/private/intake-maintainer.token"
provider = "openai"
provider_key_file = "/absolute/private/pipeline-provider.key"
agent_cli = "codex"
agent_program = "/absolute/bin/codex"
triage_model = "PROVIDER_MODEL_ID"
fix_model = "PROVIDER_MODEL_ID"
review_model = "PROVIDER_MODEL_ID"
repository = "OWNER/REPOSITORY"
mirror = "/absolute/pipeline-mirror.git"
state = "/absolute/private/pipeline-state"
shim = "/absolute/bin/ophio-sandbox-shim"
cargo_cache = "/absolute/offline/cargo"
npm_cache = "/absolute/offline/npm"
poll_seconds = 30
paused = false
```

For Claude Code, set `provider = "anthropic"`, `agent_cli = "claude"`, its
absolute CLI path, and explicit Anthropic model ids. Provider and CLI must
match. The provider key is for model inference only and is read by the host
relay. The intake token authorizes the private maintainer API; it never goes
to the reporter listener. Keep both files private, outside the mirror, state
tree, runtime mounts, offline caches and executable aliases. Startup resolves symlinks and `..` and
refuses overlapping credentials or credentials with additional hard links,
naming the setting. The implicit provenance key's location is checked too.
The existing GitHub account needs repository contents and pull-request write
access, and checks/Actions read access. It is used only on
the host for fetch, push and PR operations. Agents receive no GitHub account,
tool bridge, signing key or other host data.

The CLI runs non-interactively in bubblewrap with a fresh home and canaries.
Triage and review see read-only snapshots. Only fix sees a writable throwaway
snapshot, with no Git metadata. The host imports ordinary files into a new
commit; agent hooks, configuration, refs and objects are never imported.
Triage writes its JSON result and a separate repro file. Fix receives only the
checked verdict and reduced repro. The host restores the frozen original test.
Review receives the checked diff, with the repro reduced, and gate flags
while running in the trusted base snapshot. Proposed instruction files and CLI settings are never loaded
as the reviewer's policy.

Tests use fully offline sandboxes: no proxy or relay socket is mounted, even
for the provider. Their provider state is disposable and requires no existing
provider configuration or login directory. Prepare Cargo and npm caches in
advance; they are mounted read-only. `cargo_cache` contains only prepared
`registry/` and `git/` caches;
`npm_cache/node_modules` contains the prepared client dependencies, including
Vitest. Use dedicated caches without personal credentials. Cache locks and npm
logs go to disposable writable folders. Rust repros are registered both in the
checked snapshots and final commit, including private unit tests and nested
integration tests; Vitest results must report executed tests. Missing tools,
missing cached dependencies, compilation failures and empty repro selections
stop the report; missing cached dependencies stop it as our own trouble,
without telling the reporter it wasn't reproduced. A process past its time
stops the report as a spent budget, never a hostile one. Processes have a
10-minute stage cap, 600 CPU seconds, 8 GiB
address-space cap, a process cap and bounded output. No build runs in the
process that holds the provenance key.
Source stays read-only during test execution. Each command has fresh writable
build output and cache-lock folders, so a repro cannot poison another suite's
compiled tests. Both workspace Rust tests and client tests run when present.
Aggregate cgroup limits also cap the whole process tree at 8 GiB, four CPU
cores and 256 processes, with swap disabled. Delegated cgroup v2 memory, CPU
and process controllers are required; there is no resource-limit fallback.
The delegated unit must keep the companion in a `supervisor` subgroup
(`Delegate=yes` and `DelegateSubgroup=supervisor`); limited jobs are siblings
under the empty delegated parent. This lets the kernel apply memory limits to
every job without moving or limiting the companion itself.

The aggregate writable-filesystem defaults are 16 GiB and 250,000 entries per
command, including the initial writable workspace and every writable bind.
`Limits` exposes `disk_bytes` and `disk_entries`; configure them with:

```toml
[pipeline.limits]
disk_bytes = 17179869184
disk_entries = 250000
```

These defaults leave several GiB for real Cargo builds. Anonymous tmpfs mounts
(root, home, `/tmp`, `/run` and shared memory) split the byte ceiling into
individual kernel caps. A host watchdog checks their aggregate usage together
with all writable host binds every 100 ms plus scan time and after process-tree
cleanup. It counts allocated bytes or file length, whichever is larger, and
filesystem entries without following symlinks; nesting beyond 128
levels stops the run. Workspace, output, per-run provider state, HOME and
`CARGO_TARGET_DIR` all count. Read-only runtime files and offline caches do not.
A full scratch mount, exceeded byte/entry budget or unverifiable usage stops
with `BudgetSpent`, without a hostile strike. Nested user namespaces are
disabled so a child cannot replace the capped mounts.

Writable host binds must survive for trusted import, and a full Cargo build
need not fit in memory, so those use the watchdog rather than tmpfs. Before
starting, each writable host volume must have the byte budget plus 512 MiB
available, plus the entry budget and 1,024 spare inodes where the filesystem
reports a fixed inode capacity. Falling below either reserve stops the run.
Volume-consumption checks also detect unlinked open files, but concurrent host work can trigger a
conservative stop. This sampled limit permits bursts between scans; use a
dedicated quota-backed volume or VM for a strict host-disk ceiling. No writable
sandbox or build output is reused by the next command.

## The owner's review

Needs you gets one prominent item: “A fix for report A7K3QX is ready”. It shows
the area, repro path, changed files and lines, each gate's result, sensitive
change flags and conflicts with other reports. Open its PR to inspect the
test and diff. Merge uses the existing phone unlock and person-only GitHub
command. Close settles the request and the report becomes `Declined`. Closing
the request leaves the PR available for manual follow-up.

Owner flags are carried into review and do not prevent opening a PR. The
orchestrator corpus driver observes these cases at the flag boundary before
publication; it does not prove a terminal rejection for owner-flagged attacks.

Through merge, the merge train, agent GitHub tools and Merge together all
refuse `report/*` PRs and PRs containing a recorded report-fix commit, including
copied heads, composed branches and resumed batches. Repository, PR and commit
identities stay in the companion's durable store across restarts. Commit
inventories are capped at 100 and incomplete histories hold merges in a
repository with report fixes. No companion operation enables GitHub auto-merge;
report CI and the owner merge gate refuse it when already enabled. The
individual owner Merge action requires the open gated review and passing
isolated CI at the same head. A system or channel caller cannot use it. Local programs started by the
companion cannot use the owner's merge or close controls. The worker's forge
interface only observes merges; it has no merge authorization method.

Merged fixes wait for a running companion build containing their merge commit.
Waiting for that build does not consume the execution-time budget, including
across restarts. Intake durably completes the claim when the worker records
a merge or a terminal outcome, so expired leases on those fixes do not displace new reports.
Admission uses server-side completion and live leases, without sending local
run history. Older maintainer clients can still supply a bounded exclusion list.
The build id is the executable's SHA-256 prefix, as in the updater. A clean
source build records its Git commit;
release builders should set `OPHIO_SOURCE_COMMIT` to the full checked-out
commit. A build without source identity publishes nothing. An ancestry check
precedes `Fixed { build }`, signed provenance and the Owner-ring feed entry;
then the report becomes `Shipped { build }`. Several fixes can join the same
Owner-ring build. No installer or application signing is invoked.

Widening is the owner's manual action on the intake host:

```sh
ophio-intake admin --data /absolute/intake-data widen BUILD early
ophio-intake admin --data /absolute/intake-data widen BUILD everyone
ophio-intake admin --data /absolute/intake-data halt BUILD
```

## Pause, limits and recovery

`ophio-intake admin --data /absolute/intake-data pause true` stops admission
before the next stage. A failed switch read also stops admission. Locally,
create `PAUSED` in the pipeline state directory; remove it to clear that
override. `paused = true` also sets the persistent local pause. Clearing that
persistent setting requires the trusted `Orchestrator::set_local_pause(false)`
API; restarting never clears a budget-triggered pause. The intake switch still
wins. Polling is 5–60 seconds, default 30. Shutdown cancels active work promptly.

An optional `[pipeline.budgets]` table accepts all `BudgetConfig` fields listed
above. Defaults remain six model stages, 30 minutes of active work, 80,000
input/20,000 output tokens per report and the daily limits above. Trusted CI
and owner waiting do not consume active wall time. Reservations persist before
execution. Unknown completion remains charged and is never retried. There is
one fix attempt per report. Agent work and external mutations with unknown
outcomes are never repeated. Read-only intake, GitHub and CI polling failures
retry with persisted attempt counts and capped exponential delays. The default
is five retries, starting at five seconds and capped at five minutes; the sixth
consecutive failure stops the run. Successful polling clears the failure count.
Triage reads happen before reserving a model attempt.

`state/pipeline.sqlite` holds private checkpoints, signed release and stop
provenance, enum-only operator events and quarantine strikes. The dedicated
provenance key lives beside it, never in a sandbox. `state/relay/` retains
metadata-only relay audits, including usage and refusals. No operator event,
PR title, PR body or provenance statement contains report wording, trails or
crash messages. Checkpoints contain private intake data and repro bytes and
must stay private. Three hostile strikes locally ban further pipeline runs
for that reporter. Canaries quarantine immediately; a budget stop does not
count as a hostile strike.

A host lock permits only one worker per state directory. Leases renew during
work; a lost lease stops the stage. On restart, completed stages resume from
their checkpoints. A stage interrupted after its intent was saved stops with
signed `Interrupted` evidence; it is never executed again. Inspect that stop
and any external PR/feed outcome before a manual follow-up. Old sandboxes are
discarded. Questions use only contract templates and wait in a durable state.
The reporter can answer through the usual report controls; a maintainer can
use `Client::answer_question` on the private intake API. Answers resume triage,
with at most four answered questions JSON-escaped inside the same untrusted
fences as the bundle. Answers never enter fix or review prompts. Question waits
do not consume active wall time. Unanswered questions expire after seven days
by default; the deadline persists across restart. Expiry closes the question
and declines the report for insufficient information.

Override recovery settings with an optional table:

```toml
[pipeline.recovery]
question_timeout_seconds = 604800
retry_limit = 5
retry_initial_seconds = 5
retry_max_seconds = 300
```

Every stop durably schedules cleanup before returning: the worker resolves its
Needs-you item, removes pending merge authority, closes unanswered questions
and completes its intake claim. Cleanup is idempotent, resumes after restart
and runs even while admission is paused. If storage or intake is unavailable,
cleanup remains pending until it succeeds. Lost claims cannot retire another
worker's lease. Crashed, unfinished workers' leases still expire and their
reports can be claimed again. Old checkpoints are retired at startup without
sending an exclusion list.

Build the live containment test without running it:

```sh
cargo build -p sandbox --bin ophio-sandbox-shim
cargo test -p report-fixer --test live --no-run
```

Run the printed `live-*` executable in a user-namespace-capable session with
delegated cgroup controllers. Keep the build target outside any private `/tmp`:

```sh
systemd-run --user --wait --collect --pipe \
  -p Delegate=yes -p DelegateSubgroup=supervisor \
  env -u WAYLAND_DISPLAY -u DISPLAY \
  OPHIO_TEST_SANDBOX_SHIM="$CARGO_TARGET_DIR/debug/ophio-sandbox-shim" \
  "$CARGO_TARGET_DIR/debug/deps/live-<hash>" real_ --ignored --test-threads=1
```

The tests use synthetic secrets, a scripted shell agent, an offline Cargo
fixture and a loopback fake provider; they make no real model calls.
