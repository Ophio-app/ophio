# Contribution receiver

The receiver implements the engineering portion of Ophio issue #9. It prepares
an isolated, managed Git checkout through the existing authenticated control
connection. It never starts a task, enables a queue, executes setup, creates a
public fork, installs dependencies or runs a repository program.

The portable records follow Clankerhub's CH-015 `ch-import-draft-1` proposal,
pinned to upstream contract revision `7c7cc4ebabf48a64d548fb755afa5f8b63829a10`.
This initial profile supports public HTTPS, SHA-1 Git repositories using shallow
smart HTTP, on Linux with `openat2` (kernel 5.6 or newer). Windows acquisition,
SHA-256 repositories, authenticated/private sources, redirects, submodules,
symlinks, LFS and non-ASCII paths are unsupported. Unsupported requirements block
preparation. An existing unmanaged checkout is never adopted or reset.

## Authenticated interface

These operations use the existing `ClientFrame` command/query envelopes, device
authentication, command deduplication and admission deadlines. The
[delivery adapter](contribution-delivery.md) adds a user-opened phone link and
desktop QR fallback, resolved through this authenticated connection. There is
no new public endpoint, localhost listener or CLI command.

| Operation | Result and authority |
| --- | --- |
| Query `contribution_delivery` | Fetch the exact digest-bound handoff referenced by a user-opened link; requires Manage and host feature `contribution_delivery_v1`; grants no trust or execution authority |
| Query `contribution_capabilities` | Supported contract, transport, host, companion version, optional compiled source revision, capabilities and resource ceilings; requires Manage |
| Command `review_contribution` | Accept exact packet/handoff JSON strings and locally selected host/root IDs; return an opaque operation ID, exact byte digests, validated context and structured blockers; requires Manage |
| Command `prepare_contribution` | Accept the operation ID and a separate local `ContributionApproval`; validate fresh independent evidence and acquire/reopen the workspace; requires Manage and Library write |
| Command `set_contribution_root` | Save a whole writable Library root by its opaque ID as the contributions default; requires Manage and Library write, through the same person-only gate as preparation |
| Command `cancel_contribution` | Revoke the operation's pending trust/acquisition; preserve all files and registered projects; requires Manage and the same principal |
| Query `contribution_operation` | Current structured operation state; same authenticated principal and Manage |
| Query `contribution_policy` | Read one pinned policy as verified UTF-8 text before approval; same principal and Manage; requires `policy-preview` |

`prepare_contribution` first returns admission with `acquiring` or `verifying`.
The existing `CommandProgress` event and `CommandStatus` query carry its final
`ContributionView`. Callers must follow the **returned operation ID**: concurrent
packets can converge on an earlier operation. The original command receipt
remains an admission receipt on replay. Unknown result codes must fail closed.
`execution_authorized` is always false. Cancellation can report
`interruption_pending: true` until the worker acknowledges stopping.

Context exposes the reviewed repository/revision, policy references and optional
task reference. It contains no native workspace path, credentials or executable
instructions. `project_id` identifies the registered local project. Imported
instructions remain data until a separately authorized task uses the project.
OI-02 owns task contents, task-digest binding, model requirements, receipt exchange
and task creation. `task-exchange-draft-1` is deliberately **not advertised** here:
a packet requiring it returns `compatibility_unsupported` until that feature is
implemented. A task reference alone does not import or start a task.

## Recovery details and choices

Hosts advertising `workspace-recovery` provide the additive receiver contract
for issue #148. Callers must check this capability before sending
`prepare_contribution.workspace_choice`; older hosts may ignore unknown fields.
The phone presentation is developed separately in PR #145. These are shared
Rust/protocol changes for both Android and iPhone, with no new native command.

| State | Bounded receiver detail |
| --- | --- |
| `workspace_conflict` | `conflict`: changed and untracked file counts, branch, revision, and the existing managed `project_id` when its binding and registration still match |
| `disk_full` | `space.needed_bytes` (the 2 GiB acquisition reserve) and `space.available_bytes` (the measured free space, capped at the JSON safe-integer ceiling) |
| `compatibility_unsupported` | `missing_capabilities`: at most 18 validated identifiers; unsupported protocol versions and hash formats use receiver-owned labels, never the raw value |
| `acquiring` | `received_bytes` on the operation query: actual response-body bytes, including Git framing and the advertisement, bounded by the 256 MiB transfer limit |
| Unsupported host | `contribution_capabilities.unsupported_reason`: `unsupported_platform` or `safe_filesystem_unavailable` |
| Trusted catalog review | Optional `context.display_name`, supplied only by independently checked local approval; at most 120 UTF-8 bytes, with control, bidi-control and path-separator characters rejected |

Counts compare files and the staged index with the originally imported tree,
including deletions, mode changes and staged-only edits. Inspection never runs
Git, hooks or repository programs. Counts are null when safe bounded inspection
is unavailable (including older manifests without original object IDs, or
unsupported index forms); the phone must not render null as zero. Branch and
revision are optional as well. No native paths or raw server diagnostics are
added to these results. Details belonging to an earlier blocker are cleared on
state changes. Progress is queried while a worker runs; a restart reports
`reconcile_required`, not invented resumed bytes.

A fresh `prepare_contribution` approval can include one of these choices:

- `{"choice":"use_existing","project_id":"…"}` opens that registered managed
  workspace unchanged, even after local branch/policy edits or a newer release
  creates a conflict. The result carries the **existing** context, not the
  proposed release's policies. It rechecks ownership, root and workspace
  bindings, grants and project registration. The previous workspace's saved
  root must still match the current Library authorization, path and inode;
  reusing its root ID at a new location cannot authorize the old workspace or
  expose its conflict details. No new policy is adopted and no task is authorized.
- `{"choice":"create_separate"}` acquires a sibling under the same selected
  root, returning its own operation and project IDs. Repeating the choice for
  the same parent operation converges on that sibling. The original reservation,
  files, index and queue remain intact. The ordinary retention, concurrency,
  acquisition and approval limits still apply.

Capabilities return eligible `root_ids` and the saved `contributions_root_id`.
The phone can designate a folder with `set_contribution_root`, using an existing
whole writable Library root. The saved selection is bound to the root's device
and inode and disappears from capabilities if it is removed, restricted or
replaced. This command does not add filesystem access or create arbitrary native
paths. The input's explicit `root_id` remains authoritative for each import.

## Trust and freshness

### Reading rules before trust

Hosts advertising `policy-preview` accept `contribution_policy` with an operation
ID and an exact policy path from its reviewed handoff. They return
`{policy, size_bytes, text}`. The text is inert data: its SHA-256 matches the
handoff, but that does **not** authenticate the publisher or grant local trust.
The phone displays literal text, including HTML, without rendering links or
loading external resources. Both Android and iPhone use this shared UI and
protocol; no native plugin change is required.

The current smart-HTTP profile fetches the pinned commit's pack into memory to
resolve its tree. It verifies every policy pin and the existing 256 KiB/file,
2 MiB total limits, then returns only the requested file. Nothing is written to
the workspace, journal, project list or queue. Each Read can fetch the pack
again; this is not a policy-only network transfer. Preview shares the two
acquisition slots and network/object limits, with an 18-second deadline inside
the phone's 20-second query timeout. Invalid UTF-8 and binary control characters
are refused. Expiry, cancellation, principal/grant changes and root identity or
authorization changes are checked before and during fetching/decoding. Only
paths named as policies can be read.
Reviewing a canceled handoff again creates a fresh review operation after any
acquisition has acknowledged stopping. The old operation stays canceled, so
an earlier preview cannot be revived by the fresh review. No approval carries
over; preparation still needs a new approval and reconciles retained work.

### Remaining authentication dependency (#147)

Automatic reopen is still blocked by the publisher contract. Clankerhub's
`ch-import-draft-1` explicitly requires an independently authenticated decision
binding **both** the packet bytes and current catalog handoff bytes. It specifies
neither a verification endpoint nor a signature algorithm or trusted public key.
Its `CatalogAuthority` is separate trusted input, and `buildImport` creates an
unsigned packet in the browser. Inspected upstream `main`:
`47f8f6969bf0df6093c4745502082baa332ac8e1`.

Before replacing the manual approval, Clankerhub and Ophio need an authenticated
authority that binds packet digest/ID/expiry, publisher, game/release, exact
repository/commit and handoff digest to a current, non-revoked listing. The
receiver must obtain that authority independently of portable input, check it
again on resume and fail closed when freshness is unavailable. A signed listing
needs an agreed trust anchor, rotation/revocation rules and freshness source.
Remembered rule trust can then be stored separately, scoped to the principal,
publisher, repository/commit and policy set, host and root identity, with explicit
revocation and changed-role reporting. Caching today's manual approval or merely
extending its lifetime would bypass the required current-listing check.

Policy preview addresses the rule-reading part of #147; it does not complete
publisher verification or remembered trust. The manual profile below remains.

### Manual profile

The packet cannot carry local approval. The separate Manage-authorized approval
binds the exact raw packet SHA-256, catalog-approved handoff SHA-256, authenticated
publisher identity, game/release, repository and immutable commit. It also records
the time and validity of a fresh independent packet/catalog revocation check.
In the initial manual profile, the local reviewer obtains those values through
an independently authenticated channel; copying claims from the packet is not
verification. The presentation owner must implement that review explicitly.
There is no automatic catalog authentication or revocation service in this patch.
The companion's existing person-only owner-API gate treats preparation as an
approval: an Ophio-started program may inspect/cancel an import, but cannot grant
it local trust. Paired devices still need the authenticated Manage/Library grants.

An approval lasts at most five minutes and cannot outlive its packet. Packets
last at most one day; future, expired and excessive-lifetime packets fail.
Each resume/reopen needs fresh approval. Changing the host, configured root,
root inode, packet bytes, source revision or policy set invalidates reuse.
The selected Library root must permit the whole directory. A root limited to
selected paths cannot authorize acquisition, and narrowing a root while a worker
is running stops that worker before further acquisition or registration.
Cancel revokes the pending approval. Grant revocation and shutdown are checked
while acquisition runs and again before registration. A stored `prepared` result
records completed preparation, not continuing execution authority; querying it
after approval expiry returns `freshness_unknown`.

Each JSON record is bounded before parsing. Unknown fields are rejected; duplicate
keys use the last occurrence consistently, as the upstream contract specifies.
Raw bytes are hashed before any reserialization. Repository URLs are exact,
canonical ASCII HTTPS identities: no credentials, alternate ports, dot segments,
escapes, query parameters, fragments or implicit `.git` rewriting.

## Acquisition and workspace integrity

The receiver implements the bounded subset of
[Git smart HTTP](https://git-scm.com/docs/http-protocol) and
[pack format](https://git-scm.com/docs/pack-format) it needs. It uses no Git or shell
subprocess in production. The HTTPS client disables proxies, redirects and HTTP
content decompression. Every DNS answer must be public; the checked addresses
are pinned into that client's resolver for every subsequent connection. No
credentials, inherited Git configuration, hooks, templates, filters, remote
helpers, submodule or LFS requests participate.

It requests only the reviewed commit at depth one. Pack checksums and every
object ID use collision-detecting SHA-1; the pinned commit's tree must resolve
completely. REF/OFS deltas have bounded expansion and depth. Policy blobs must
match their SHA-256 pins before registration. Materialization rejects symlinks,
gitlinks, `.git` entries, `.gitmodules`, case collisions, path escapes, Windows
aliases and LFS pointers. Files are written literally; attributes cannot run a
filter. The receiver writes a minimal Git object database, index and contribution
branch, with repository hooks disabled. The result remains an ordinary Git
checkout for separately authorized work.

Linux writes use held directory descriptors, no-follow/beneath/no-mount-crossing
resolution and exclusive creation. Root/workspace device and inode identities
are checked before reuse and registration. Reopening reads policy files without
following links or accepting hard links, reports dirty/untracked work, and never
checks out, resets, cleans, switches branches or updates an existing queue.
Changed policy bytes or a changed head/config require reconciliation. A checkout
provides separation from other work; it is not an execution sandbox.

Clean verification requires every manifest file to remain a regular file with
one hard link. Replacing a tracked file with a directory reports dirty on reopen
and prevents registration after interruption. Index verification compares staged
paths, object IDs and modes while disregarding Git's stat cache and optional
cache extensions. SHA-1 index formats 2, 3 and 4 are supported; split/sparse
indexes and unmerged or intent-to-add entries require reconciliation before an
unfinished registration can complete. Staged changes are preserved on reopen.
Verification never rewrites the index. Older saved manifests remain readable
and can be verified after an ordinary `git status` refresh. New manifests retain
original object IDs to count staged differences during conflict inspection.

| Bound | Enforced limit |
| --- | --- |
| Each portable JSON record | 65,536 bytes, nesting 16 |
| Policy | 256 KiB per file, 2 MiB total |
| Network | 256 MiB response bodies per attempt; advertisement at most 1 MiB |
| Objects | 64 MiB per object; 512 MiB aggregate expansion including intermediate delta data; delta depth 32 |
| Checkout | 512 MiB expanded files; 20,000 entries; depth 32; paths 512 ASCII bytes |
| Disk | At least 2 GiB available before acquisition and at write checkpoints, covering staging plus a free reserve |
| Time | DNS 5 seconds, connect 5 seconds, read idle 15 seconds, entire operation 120 seconds |
| Concurrency | Two acquisitions/verifications on the selected host |
| Recovery retention | At most three acquisition attempts per preparation; 1,000 receiver journal records |

Counters apply to actual streamed/decompressed bytes. OS filesystem calls can
remain in progress while cancellation is recorded; the operation keeps its slot
and reports interruption pending until its worker returns. Limits are receiver
policy; no portable field can raise them.

## Journal and recovery

Packet IDs are scoped to the independently authenticated publisher; changing the
raw digest under an existing ID returns `id_conflict`. Workspace reservations
are scoped to the initiating principal, host, selected root and exact repository.
The preparation fingerprint includes the source revision, policies and task
reference. Reservations serialize before side effects. Different packet IDs
with matching preparation inputs converge on one operation and project.

The journal records intent before creating an operation-owned directory. A
verified file manifest is durable before project registration. Project creation,
its initially paused queue and the journal's project identity commit in one
SQLite transaction. On restart, an unfinished worker becomes
`reconcile_required`. With renewed approval, a complete manifest is reverified
and registration reconciled without fetching again. An incomplete attempt stays
untouched; a bounded retry uses a new exclusive directory. Completed
registrations and queues are never rewritten by cancellation or recovery.

Registration enforces the same snapshot size budget as local project creation,
inside the project/journal transaction. A full budget leaves the operation at
`reconcile_required`, with its complete checkout retained and no partial project
or queue committed. Once capacity is available, fresh approval retries that
registration without fetching again. A review refused before a root was ever
bound can establish its first binding when the temporary condition clears (for
example, when `issuedAt` is reached or the selected root becomes writable).
Reviews with an existing binding continue to reject replacement roots.

All interrupted directories are retained. The receiver performs no automatic
cleanup: an interrupted attempt may have acquired user work before restart.
After three incomplete attempts or the journal ceiling, a person must inspect
retained resources. No data-deletion interface is added.

## Evidence and downstream gates

The task started from Ophio `8aacbe13d2e95e28263fe7b7fbd2021c84c25964`, containing
the accepted prerequisite source available on `main`. CH-015 implementation and
its owned fixtures were inspected at merged commit
`7c7cc4ebabf48a64d548fb755afa5f8b63829a10`; Clankerhub's inspected `main` was
`d47de2600a93dbd18a8f5915403360f821501eb0`. CH-015's technical contract has no
prerequisite visual/device acceptance. Its downstream acceptance remains open.
The actual authoring run recorded Codex `gpt-6-astra`, effort `xhigh`.

The resumed 2026-10-12 run verified the same model/effort in its active run record
and merged `origin/main` at `873278e01585434683e25c1cd34a4c80ff034804` into the
existing contribution branch. Work used an isolated worktree so unrelated edits
in the original checkout were preserved. An assertion against that `main`'s
`protocol::Command` reproduced the absence of review/preparation commands.
Installed `ophio --version` still reported `0.1.0`, and `ophio projects --help`
still offered only local project registration/settings. The installed build's
exact source and phone version remain unverified; nothing was installed.

Before edits, installed `ophio projects --help` exposed local registration but
no import, and inspection of `protocol::Command` confirmed no receiver. The
installed application reported version `0.1.0`, build `c0ad17bd3e3a`; its installation
record did not identify an exact source commit. This source change is not an
installation/update, and that installed build is not claimed to contain it.
`source_revision` remains null unless the build explicitly supplies
`OPHIO_SOURCE_REVISION`; a null value must not be presented as verified provenance.

Owned Rust fixtures construct real Git objects/packs and run the production
validator, decoder, workspace writer, SQLite journal and command boundary.
They cover hostile paths/objects, delta formats, byte tampering, expiry, trust,
limits, concurrent IDs, cancellation, dirty reuse, changed roots/policies,
symlink substitution, and restart at the manifest/registration boundary. A Git
client independently reads the produced checkout. Loopback HTTP fixtures test
wire framing and streaming bounds without contacting an account or game project.
Fixture publisher approval, clock and source transport are synthetic; they do
not prove a real catalog/reviewer relationship or Internet DNS environment.

The initial 2026-10-10 verification environment was Linux x86_64, Rust/Cargo 1.99.0 and
Clippy 0.1.98. The affected Rust suite passed 575 tests, including 14 receiver
tests; the client suite passed 1,287 tests in 207 files. Client typechecking,
host compilation and workspace-wide Clippy passed. Native Windows/Android
runtime tests and a real phone/catalog round trip were not run. Free-space
failure tests inject available-byte counts while retaining the real workspace
writer and journal; they do not fill the computer's disk.

On the resumed baseline, Linux `7.2.7-arch1-1` x86_64, Rust/Cargo `1.99.0`,
Node `24.19.0` and npm `11.17.0` passed 1,149 affected Rust tests and 2,043
client tests in 304 files. A new restricted-root regression failed before the
fix (`awaiting_local_approval` instead of `root_changed`) and passes afterward.
It covers review, admission and revocation during a suspended fetch. Duplicate
review checks now revalidate expiration and the root binding. Admission and
restart use small journal guards rather than loading every retained manifest;
the 15 receiver tests passed again after that change.

The resume ran these commands (build targets, temporary fixtures and logs were
under the task scratch directory; desktop/session variables were unset for Rust
tests):

```sh
cargo test --locked -p protocol -p domain -p services
cargo test --locked -p services contribution::
cargo clippy --locked -p protocol -p domain -p services --all-targets -- -D warnings
cargo check --locked -p companion
cargo test --locked -p companion --lib person_only::tests
cargo clippy --locked -p companion --all-targets -- -D warnings
(cd client && npm ci --ignore-scripts --no-audit --no-fund)
(cd client && node scripts/protocol-barrel.mjs)
(cd client && npm run typecheck)
(cd client && npm test)
python3 scripts/licenses.py --artifact linux --check
```

Clippy, host compilation and the companion's person-only approval regression
passed. Host compilation first caught the missing
exhaustive owner-API classifications after the merge; those now preserve the
person-only approval boundary described above. `cargo fmt --all --check` and
`git diff --check origin/main -- . ':!client/src/protocol/generated'` passed;
generated TypeScript retains ts-rs formatting, including its trailing spaces.
The first GitHub Linux run failed before building because the separate Tauri
workspace lockfile lacked the receiver's dependencies. A targeted
`cargo update --offline --manifest-path client/src-tauri/Cargo.toml -p services`
added the same pinned dependency without upgrading other packages.
`cargo fetch --locked --manifest-path client/src-tauri/Cargo.toml` then passed.
`graphify update .` refreshed the AST graph without an API call; it reported
155 unsupported data files with no extracted nodes. The pre-push source scan
initially rejected a personal repository URL in this document; the sanitized
contract revision above retains the technical binding without that identifier.

The Linux license inventory reports 462 components, zero failures and the same
existing MPL review item. Shared Rust and generated TypeScript contain the new
interface; this change needs no Android/Swift-specific implementation. The phone
presentation remains OI-01-U's work, and neither phone's runtime is qualified by
these host/client unit tests.

The PR records exact verification commands and results for this revision. The
repeatable focused commands are:

```sh
cargo fmt --all --check
env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
  cargo test --locked -p protocol -p domain -p services
cargo clippy --locked -p protocol -p domain -p services --all-targets -- -D warnings
python3 scripts/licenses.py --artifact linux --check
```

The 2026-10-13 review follow-up verified Codex `gpt-6-astra`, effort `xhigh`,
and reproduced all four reported defects before changing production code:
registration exceeded the snapshot budget, a directory replacing a tracked file
passed as clean, a normal `git status` made a clean checkout appear dirty, and an
unchanged not-yet-valid packet could never establish its first root binding.
The initial receiver run had 15 passing and four failing tests. After the fixes,
all 21 receiver tests passed, including staged-only changes and malformed index
coverage. The broader run, including additional rechecksummed/truncated version-4
index cases, passed 1,157 tests with no failures or skips. It used the branch with
upstream through `9d576031207761719a2e23bf5be54fc5355faf15`.

Commands for that review follow-up:

```sh
cargo test --locked -p services contribution::
cargo test --locked -p protocol -p domain -p services
cargo clippy --locked -p protocol -p domain -p services --all-targets -- -D warnings
cargo fmt --all --check
git diff --check
bash scripts/source-candidate.sh
python3 scripts/earlier_names.py
graphify update .
```

Clippy, formatting, diff and source/name scans passed. The graph's AST update
completed; 155 data files had no extracted nodes and semantic community labels
were not refreshed. The environment remained Linux `7.2.7-arch1-1` x86_64,
Rust/Cargo `1.99.0`, with Git `2.56.0`. Builds and fixtures used the task scratch
directory, six build jobs, no incremental state or debug information; Rust tests
unset desktop/session variables as above. No installed build, phone, live catalog
authentication, private source support or native Windows qualification was added.
The review fixes do not change the shared protocol or require native phone code.

The later 2026-10-12 continuation verified Codex `gpt-6-astra`, effort `xhigh`,
and merged `origin/main` at `0e707c268aefd100b78f693f6341c2d6cbbfc79c`.
The receiver, Rust dependencies and generated protocol files were unchanged by
that merge. `cargo fmt --all --check`,
`git diff --check origin/main -- . ':!client/src/protocol/generated'` and
`npm run typecheck` in `client/` passed. With `TMPDIR` set to the task scratch
directory, `npm test -- src/host/catchUp.test.ts src/host/liveText.test.ts
src/host/withheldApprovals.test.ts src/screens/github/github.test.tsx` in
`client/` passed all 61 tests in four files. The earlier Rust suite was not
rerun locally for this client-only upstream merge.

The first two GitHub runs failed their `test` and `parallel` stages at
`a_claude_conversation_starts_without_the_permission_bridge`, whose assertion
rejected the bridge now supplied to Chat. Issue #114 tracked this separate
baseline failure. Its fix passed Linux CI and merged in #127; the contribution
branch then merged `origin/main` at
`3fc4c816e4ef09330e5cdee0fd8051f9ebda4ee9`. That merge changes only the upstream
composition test, leaving receiver behavior unchanged. Formatting and diff
checks passed locally; a fresh PR CI run verifies the combined revision.

`sha1-checked 0.10.0` adds collision detection from RustCrypto's maintained hashes
project; `flate2 1.1.10` was already locked and is now a direct dependency using
its Rust backend. Both offer MIT/Apache-2.0 licenses. The generated inventory
includes their supplied attribution; the existing MPL review item is unchanged.

OI-01-U (#10) owns all visible review, trust/host selection, progress and recovery
controls. OI-02-E (#11) consumes the operation/project/context boundary. CH-065
must still verify an installed source/build pair, real catalog trust/freshness,
a declared phone/computer combination, and first/repeat web-to-app handoff with
separate human acceptance. No Android/Windows runtime qualification, public
release, device installation, paid agent run or real one-click integration is
claimed by these engineering fixtures.
