# Core services

`CompanionHub::builder().data_dir(path).build().await` creates a durable Hub.
Pass configured ports through `.harness(...)` (repeatable), `.desktop(...)`,
`.media(...)`, `.library(...)`, `.terminal(...)`, and `.speech(...)`.
`.clock(...)`, `.host_id(...)`, `.retention(...)`, `.renewal_window(...)`,
`.max_upload_bytes(...)`, and `.preview_checker(...)` override defaults.
Absent adapters return typed `unsupported` errors. The default preview checker
uses HTTP(S), disables proxies and redirects, and validates private numeric
addresses or explicitly loopback-resolved localhost. DNS names other than
localhost are refused, including tailnet names; register their private IP.

Pass `.host_id(identity.host_id)` from the companion's persisted transport
identity. A fresh store adopts that id; a different id in an existing store
fails closed. Omitting the option preserves standalone hub behavior.

Configure Library roots on its port before building. `set_library_roots` applies
owner changes live and emits `LibraryRootsChanged`. `set_retention` updates the
running store's journal policy under the same exclusive hub lock. Config
persistence and path canonicalization belong to the owner composition.

`desktop_unavailable` supplies an honest capability/remedy for an absent
backend. `set_desktop` fences old input, withdraws managed observations, closes
streams with a reason and records input-release receipts before installing a
replacement. Failed release remains unknown and prevents replacement. The
companion calls these hooks only through its local owner runtime; they add no
remote authority. `refresh_readiness` publishes capability and account remedies.

The composition root owns
platform readiness, the actual private-network listener, TLS identity, native
bridge transport, and adapter selection. Nothing here launches a listener.
Call `shutdown().await` before dropping the last Hub during orderly shutdown.
It drains tracked work and closes native runs, terminals and desktop streams.
The composition stops accepting connections and drains its tool bridge first.
The data directory has an exclusive process lock. Do not open two Hubs for the
same directory. Unix data directories are set to mode 0700; the Windows
installer must place the directory under an owner-only ACL.

Enable `services/testing` to reuse `FakeClock`, `FakeHarness`, `FakeRunControl`,
`FakeDesktop`, `FakeMedia`, `FakeLibrary`, `FakeTerminal`, and `FakeSpeech`.
These are synthetic ports, with no desktop access or inference.

The running-task guard queries pending approvals by run through a partial
SQLite index. Resolved history and requests for other runs are not decoded;
the blocking check uses an existence query without loading request payloads.
Requests already carried on remain answerable but do not block the run or get
sent back again. The guard read-cost regression adds 1,000 resolved 8 KiB
requests; store tests check that SQL work also stays constant as unrelated
pending requests and resolved history grow.

The Linux idle-hub regression counts voluntary context switches on a runtime
with timers enabled and no I/O driver. Tokio's I/O runtimes share a signal pipe,
so child exits in parallel tests can wake even an idle reactor. The test drives
child exits on another thread during its observation window and keeps
the limit of three wakeups over at least 500 ms; hub timer polling still fails.

The executable composition in `companion::compose` installs the real harnesses,
Library, PTYs, tile encoder and HTTP preview checker. Desktop is owner-enabled
and selected from explicit Linux session endpoints. Speech, channel workers and
Windows platform composition remain dedicated integration seams there.

## Admission, results, and recovery

An admission receipt is immutable. Retries return its original content with
`duplicate`, including across restarts. Reusing an id with different effect
payload, revision guard, expiry, or principal fails with `conflict`; changing
only informational `issued_at` does not create a new effect.

Local changes, admission, work records, and journal facts commit together.
Native effects happen afterward. `CommandProgress` carries the later stage,
typed result, and typed error. `Query::CommandStatus` returns the immutable
receipt alongside the latest execution fields. Consumers must use these
execution fields to obtain asynchronous lease, terminal, capture, and Library
results. For example, a Library save conflict returns `Saved { Conflict { .. } }`
with the current content; it does not overwrite the file.

The optional progress/result fields are additive wire changes. They let the
transport acknowledge durable admission immediately without mutating that
acknowledgement when a native operation finishes. The generated TypeScript
bindings are included with the contract commits.

The SQLite store uses WAL, FULL synchronous mode, foreign keys, and forward-only
schema versions. Before a migration, SQLite's backup API writes a separate
backup including committed WAL data. Journal retention deletes only a prefix.
Snapshots read projections and the reflected journal head in one transaction.
Subscribe receives the backlog and an already-attached live receiver. Follow
`api::Subscription`: send backlog, announce its head, then discard live durable
events at or below that head. A broadcast lag requires resubscription. Grant
filtering can legitimately omit sequence numbers; the journal cursor remains
host-global. An old cursor yields an explicit retention gap.

Startup changes the boot epoch, invalidates bridge tokens and old observations,
expires pending approvals, and parks every unsettled run/command for review.
There is no automatic native redispatch. Native acknowledgement, process exit,
and child cleanup remain distinct facts. A completed parent with active or
unknown children retains the writer slot as reconciliation-required.
`RetryAsNewRun` is refused while the old writer is unresolved: explicitly mark
its reviewed outcome completed/failed, then issue a revisioned Resume.

An agent's process exit is evidence, not confirmation that its completion was
saved. The tracked event worker keeps the masked exit and outcome in memory
and retries failed finalization writes once a second. It retires the process
control and watchdog while it owns recovery, so storage trouble cannot turn a
finished agent into a watchdog interruption. Lost event streams retry visible
reconciliation instead. Input revocation is attempted even when completion
cannot be saved. Retry bookkeeping carries existing queue permissions across
its revision updates, preserving continuations authorized during recovery.
Shutdown cancels the retry delay; if the computer stops before the exit can be
saved, startup leaves the unsettled run for reconciliation.

A queue authorization covers one exact task revision once and records its
queue revision and remaining count. Reordering or insertion withdraws every
waiting item's authorization. Editing an item's prompt or dependencies, or
removing or archiving its task, withdraws only that item's and its transitive
dependents'; the rest keep theirs, carried to the new queue revision. Renaming
a task changes nothing the person authorized and carries its permit.
Pause and admission serialize in the same transaction path. A pause receipt
names an already-admitted writer. Failed or uncertain dependencies block
progress. GUI queue items require an explicit supervised start. Reconnecting,
subscribing, querying, and locking never start a queue item.

## Identity and managed control

Only `Principal::LocalOwner` opens pairing. One 32-byte random ticket is open at
a time, expires after five minutes, and is consumed transactionally. Only its
SHA-256 is stored and comparison uses `subtle` on fixed-size digests. The first
enrolled device receives owner grants. Subsequent devices receive view, tasks,
approvals, and Library read; they receive no terminal, input, computer-use,
transfer, Library write, or admin authority by default. Channel identities
receive only tasks and approvals. Channel callers cannot use desktop, terminal,
Library, transfer, or admin routes even if a stale transport passes broader
grants. Calls recheck persisted revocation and intersect current grants with
those supplied by authentication.

The arbiter owns one sink and lease per target, and checks identity, boot epoch,
generation, current geometry, monotonic sequence, and monotonic deadline before
input. Human renewals use `renew_lease`; agent leases may be renewed only by their
recorded supervising device through that same route. The default window is
three seconds. A 50 ms background tick checks expiry, and every input also
checks it directly. Input, release, takeover, and managed capture delivery
serialize on the target. Native injection is bounded by a 500 ms timeout;
release acknowledgement is bounded by two seconds. A timeout closes admission
and reports unknown. These are service limits, not measured native platform
latency guarantees; a backend must qualify cancellation and release semantics.

`BridgeConfig::new(executable, socket)` uses the port-defined environment names
for a per-run token and socket. Tokens are random, persisted only as hashes,
and revoked before Stop attempts native interruption. `spawn_config(run)` is
available as a Hub method and a module helper. The bridge requires a fresh
observation after a lease generation change. Permission and human requests
become approvals and await an explicit decision or Stop. The server name is
`computer` and the unqualified permission tool is `permission_prompt`; the
Claude adapter supplies the native MCP prefix. A consumed bridge binding cannot
reconnect. For Claude, relay lifecycle events and broker requests share one
approval authority: the broker installs the waiting response before publishing
the approval. Disconnect expires pending approvals and withdraws run authority.
The chosen decision commits before the bridge is released to continue.

A private step records input release, managed screenshot withdrawal, prior
observations already delivered, and uncovered routes independently. Its default
coverage includes independent native capture routes, so it does not claim
privacy. Only an integrator with evidence of full native-route restriction may
supply an empty `BridgeConfig::uncovered_routes`. Input remains closed while a
private step lacks acknowledged coverage. Ending the step does not itself
restore agent authority. A mission bound supplied as `Enforced` is rejected
because the current harness port provides no evidence for that assertion.

## Transfers and artifacts

Uploads resume from the staged file length. Chunks cannot exceed 256 KiB or the
declared size; the default per-transfer limit is 2 GiB. Finish synchronizes the
staging file, checks length and SHA-256, then publishes without overwriting an
existing destination. Project paths are opened through directory capabilities,
including at finalization, so symlink/reparse escapes are refused. Collisions
use a numeric suffix before the extension (`notes (1).md`); compressed tar
extensions stay together and dotfiles retain their leading name. Artifacts use a SHA-256 path plus a durable metadata,
creator, and provenance index. File arrival never starts work.

A hash mismatch retains staging in a failed transfer until explicit cancellation.
Interrupted ordinary uploads are paused and resumable. A durable publication
marker fences the crash window around filesystem publication: if publication
may have occurred, resuming reports `outcome_unknown` and requires inspection
before cancellation/new upload. It never silently creates a second file.
Cancellation preserves already published files. Download sources are immutable
content-addressed snapshots and enforce their original Library/project grant
on attachment and reads. Download completion means the source was consumed;
it is not evidence the phone saved the bytes. Retained speech audio is WAV and
is indexed only when `keep_audio` was requested.

Artifacts are not permanent storage. An agent's pictures, videos and screenshots
expire seven days after the latest of their creation, the phone's last fetch and
the end of their task, never while one of their tasks is unfinished: running,
waiting on the person, waiting to start or with an uncertain outcome. A stopped
task whose next message waits in Next up keeps all of its pictures, even while
the queue is paused or waiting for permission. A page an agent showed expires
seven days after it was last shown or opened. Artifacts the person sent or
captured, sources carried by a prompt or queued request, after-evidence and pages
the person added never expire. Querying or downloading an expired artifact fails
with `Expired`.

When the hub starts and hourly after, it forgets what has expired, then deletes
every content-addressed blob that no artifact names, no recent download may
still read and no unfinished upload is publishing. Deleting an artifact collects
at once. The same sweep removes blobs that an interrupted preparation left
unindexed. Keep anything that must last in the project or save it on the phone;
[the collaboration guide](../../docs/collaboration.md) has the full rules.

Task timelines are currently retained indefinitely; only the event journal has
configurable count/age compaction. Windows filesystem publication and native
lease revocation still require real platform qualification. Service tests do not
certify power-loss behavior or native input cancellation on that platform.

## Remaining adapter/contract limits

- The harness port has no native-state query/resume capability that can establish
  an uncertain dispatch outcome. Startup therefore requires explicit review.
- The wire has no per-check readiness timestamp. Checks from a refresh share the
  report timestamp; network reachability is honestly unknown until the transport
  supplies evidence. The services do not inspect network credentials.
- Arbitrary native shell/tools are outside managed input/observation control.
  The supplied port cannot attest isolation, actual delegation, or mission-bound
  enforcement. Native integration must qualify those claims before release.
- The ports do not expose file-path identity or binary Library reads. A Library
  download is refused if its exact bytes cannot be reconstructed and checked
  against the supplied revision. Unsupported video annotation is refused; use
  reviewed still frames.
- The selected OS/toolchain must support bundled SQLite and the chosen native
  adapters.

## Direct dependencies

| Crate | Declared version | License |
| --- | --- | --- |
| rusqlite | 0.38.0, bundled + backup | MIT; SQLite public domain |
| sha2 | 0.10.9 | MIT OR Apache-2.0 |
| base64 | 0.22.1 | MIT OR Apache-2.0 |
| getrandom | 0.3.4 | MIT OR Apache-2.0 |
| subtle | 2.6.1 | BSD-3-Clause |
| url | 2.5.7 | MIT OR Apache-2.0 |
| cap-std | 3.4.5 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| reqwest | 0.12.28, rustls TLS | MIT OR Apache-2.0 |

Each crate's manifest declares its own dependencies. Workspace serde_json is used by domain; existing schemars is used only for its grant-table
schema coverage tests. The composition root must retain transitive dependency
notices in packaged distributions.

Force stop keeps the writer reserved until termination or an independent process
exit is confirmed. A late exit also settles a pending Force stop after an
uncertain parent turn has already recorded an end timestamp.

Contribution imports use the existing authenticated command/query boundary.
See [the receiver contract](../../docs/contribution-import.md) for the separate
local trust decision, supported acquisition profile, durable workspace identity,
resource ceilings and downstream presentation/acceptance gates.
