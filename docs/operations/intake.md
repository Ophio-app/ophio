# Private report intake

`ophio-intake` receives signed problem reports from computers and phones,
answers known issues, and serves status and release information. It runs no
AI, starts no agents, and executes nothing from a report. A separate pipeline
uses its private maintainer API. Public hosting is a separate decision.

## Run on a host

Build with `cargo build --release -p intake`. Choose a dedicated account and
a private data directory on a local filesystem, then run:

```sh
ophio-intake init --data /srv/ophio-intake
ophio-intake serve --data /srv/ophio-intake \
  --listen 127.0.0.1:8070 --maintainer-listen 127.0.0.1:8071
```

Initialization creates `intake.sqlite`, `config.json`, and
`maintainer.token`; it refuses to replace existing credentials or configuration.
On Unix the directory is `0700` and newly created files are `0600`.
`serve` requires the token file to be a regular, non-symlink file with mode
`0600`. Do not print it, place it in command arguments, or include it in logs.
Restart the server after changing the configuration or rotating the token.
The process stays in the foreground and stops on Ctrl-C. These commands do
not install or enable a service.

Both listeners default to loopback. The maintainer listener must use a
loopback address; use an authenticated SSH tunnel for remote maintenance.
Never publish or proxy `/maintainer/v1/`. The public listener has no
maintainer routes, and the private listener has no reporter routes.

## TLS and reverse proxy

Use a TLS reverse proxy for the reporter listener. Publish only these paths:

| Method | Path | Request → response |
| --- | --- | --- |
| POST | `/v1/reports` | `IntakeSubmission` → `IntakeReceipt` |
| POST | `/v1/reports/status` | `StatusRequest` → `StatusResponse` |
| POST | `/v1/reports/answer` | `IntakeAnswer` → `StatusResponse` |
| POST | `/v1/reports/withdraw` | `WithdrawRequest` → `StatusResponse` |
| GET | `/v1/releases` | Empty body → `ReleaseFeed` |

Allow `OPTIONS` on those same paths. Reporter CORS allows any origin,
`GET, POST, OPTIONS`, and `content-type` plus the three report headers;
credentials are disabled. `Retry-After` is exposed to web clients. The
maintainer listener never sets CORS headers.

Each reporter request must carry `ophio-report-key`, `ophio-report-time`,
and `ophio-report-signature`, following `protocol::reports::signing_payload`.
The signature covers the method, contract path, timestamp and exact body.
The intake enforces the contract's clock window and remembers signatures
until the signed timestamp plus that window, including across restarts.
A retry needs a fresh signature and timestamp; keep the same report id.
Only the original key may reuse that id, and the first bundle remains stored.

Configure the proxy to preserve bodies and paths, pass those headers, and
limit submission bodies to 925696 bytes (900 KiB plus 4096 bytes for the
envelope). Other reporter paths accept at most 32 KiB. Also set connection,
header, request-time and concurrency limits at the proxy to bound work
before authentication. Disable request-body logging and diagnostic dumps.

By default, socket peers determine the IP limit. When a single reverse proxy
is used, set `trusted_proxy` in `config.json` to its exact immediate-peer IP.
That peer must overwrite `X-Forwarded-For` with one client IP, not append a
chain. For nginx, use `proxy_set_header X-Forwarded-For $remote_addr;`.
Missing or multiple IPs from the trusted proxy are refused. Headers from
all other peers are ignored. Restrict access to the backend so untrusted
processes cannot impersonate that proxy. Without this setting, clients behind
a proxy share its IP bucket.

## Limits and switches

`config.json` contains the following defaults:

```json
{
  "retention_days": 90,
  "reports_per_key": { "capacity": 4, "refill_seconds": 900 },
  "reports_per_ip": { "capacity": 24, "refill_seconds": 150 },
  "requests_per_key": { "capacity": 120, "refill_seconds": 1 },
  "requests_per_ip": { "capacity": 240, "refill_seconds": 1 },
  "trusted_proxy": null
}
```

Each bucket starts full and gains one token per `refill_seconds`. Requests
consume both key and IP request buckets. New submissions also consume report
buckets. A same-key resubmission does not consume another report token.
State is persisted in SQLite. A limit returns `429` with `Retry-After` in
seconds. A banned key gets a generic `403`. Invalid authentication and
replayed signatures get a generic `401`.

The two switches are independent and persist across restarts:

```sh
ophio-intake admin --data /srv/ophio-intake pause true
ophio-intake admin --data /srv/ophio-intake close true
ophio-intake admin --data /srv/ophio-intake switches
```

`pause` sets `pipeline_paused`. Workers must read it before each stage;
claims return no work while it is set. It does not affect reporters or stop
already running workers. `close` sets `intake_closed`: submissions return
`503` and status says `paused: true`; status, answers, withdrawal and releases
remain available. The `503` body is a `StatusResponse` with `paused: true`
and an empty report list, so devices can distinguish closure from an outage.
Use `false` to reopen or resume. Neither command deploys
or rolls back anything.

Ban or unban the SHA-256 public-key fingerprint shown in maintainer results:

```sh
ophio-intake admin --data /srv/ophio-intake ban KEY_FINGERPRINT
ophio-intake admin --data /srv/ophio-intake unban KEY_FINGERPRINT
ophio-intake admin --data /srv/ophio-intake counters
```

## Maintainer API and local administration

`intake::maintainer` exports all private request/response types;
`intake::client::Client` is the async Rust client using reqwest with rustls,
no redirects, no environment proxy, and a request timeout. Read the token
file in the worker and construct the client in memory. Its debug output and
errors omit credentials and response bodies.

| Method | Private path | Types |
| --- | --- | --- |
| POST | `/maintainer/v1/reports/query` | `ReportFilter` → `ReportPage` |
| GET | `/maintainer/v1/reports/{id}` | `ReportRecord` including optional bundle |
| GET | `/maintainer/v1/state` | `Switches` |
| GET | `/maintainer/v1/counters` | `Counters` |
| GET | `/maintainer/v1/known` | `Vec<KnownIssue>` |
| GET | `/maintainer/v1/releases` | `ReleaseFeed` |
| POST | `/maintainer/v1/command` | `Command` → `CommandResult` |

Every private request requires `Authorization: Bearer …`. Token comparison
uses constant-time comparison of fixed-size SHA-256 digests. Commands cover
admission, claim, test checks (claim, record and defer), enum status, template
question, question closure, known-issue upsert and removal, release
publication/widening/halt/completion, switches, bans and an immediate
retention sweep.
Every successful command, including an empty claim, adds an audit row in the
same transaction. Failed commands roll back without an audit row.

Queries filter by complete `ReportProgress`, signature key, a recorded check's
failure site and exclusive `since` timestamp. Pages are oldest first, capped at 200, with an exclusive
`next` cursor. Lists omit bundles. A record includes its duplicate group's
root and count. Reporter status never includes another key's reports.

A claim takes only a report the owner has admitted; nothing else reaches the
fixer. `Admit` marks one open report the fixer hasn't finished, and admitting
it again changes nothing. A closed or finished report is refused (`403`) and
an unknown one is `404`. Reports received before admission existed start
unadmitted.

Claims are atomic and return the bundle plus a worker id and lease deadline.
Lease durations are 1 ms through one hour. An expired lease becomes claimable
again; the worker must finish before expiry or arrange a fresh claim. A lease
is scheduling, not an authorization credential: all token holders are trusted
maintainers. Status changes do not renew leases. Consumers should make their
stages idempotent. Questions accept only choices in their template; answer
words are cut to the contract limit. Closing an unanswered question removes
it from the reporter view.

## Test submissions and checks

A report can carry a failing test instead of, or as well as, words: the
bundle's `test` holds the commit of the reporter's build, the test's kind
(`rust_integration`, `rust_unit` or `vitest`), its path and source, an
optional suggested fix as a unified diff, and the program and model that
say they wrote it. Every field is the reporter's claim. The intake checks
only their form, with the same rules the fix pipeline uses: a full lowercase
40-digit commit; a relative path of plain segments, none hidden, in the
folder its kind runs from (`crates/<crate>/tests/…`,
`crates/<crate>/src/…_tests.rs`, or `client/src/…test.ts(x)`); a test of at
most 8 KiB and a fix of at most 32 KiB, with no NUL, direction controls or
other invisible format characters (zero-width characters, tags, variation
selectors);
and a model name of at most 160 characters. Anything else is refused like
any invalid bundle. The whole bundle still fits the report limit. Words,
trails and extras may be empty, and signing, receipts, status, withdrawal
and retention work as for any report.

The fixer checks each test submission when it arrives, whether or not the
owner chose it. `ClaimCheck { worker, lease_ms }` atomically takes the
oldest open test submission with no recorded check and no live check lease,
with its bundle. It is independent of admission and of the fixer's own
claim and lease, offers nothing while `pipeline_paused` is set, and never
offers a withdrawn or closed report. Check leases are 1 ms through one hour.
`RecordCheck { report_id, worker, check }` records a `TestCheck` only by the
worker holding the live check lease; recording the same check again changes
nothing, and a different one is refused. `DeferCheck { report_id, worker,
retry_ms }` lets that worker put a check off for up to a day, so nobody is
offered it until then. Each is audited like every other command.

A `TestCheck` says what running the test contained showed at the reporter's
build (`at_build`) and at main's tip (`on_main`, with that commit), and,
when it failed inside our tree, where (`failure_site`, `path:line`). Each
side is `fails`, `fails_without_assertion`, `passes`, `does_not_build`,
`no_test_ran`, `timed_out`, `over_limits`, `crashed`, `unreadable_output`,
`refused` with a reason
(`unknown_commit`, `not_on_main`, `path_taken`, `no_such_folder`,
`invalid_test`, `hostile_signal`) or `not_run` with a reason
(`missing_dependencies`, `runner_unavailable`, `submission_refused`). The
test controls its own output and exit, so `fails` means its run reported an
assertion failure in the harness's own shape, not that a bug is proven.
`agent_block` says when Look into it can't use the test because no agent may
be shown its code (`unreducible` or `long_literal`).
Records carry their check; lists carry it without the bundle. Checks are for
the owner only: reporter status never includes or follows from them, and no
reporter route returns them.

Examples of local operations (all database writes use the same command
implementation as HTTP):

```sh
ophio-intake admin --data /srv/ophio-intake list
ophio-intake admin --data /srv/ophio-intake list --signature 'empty@/work/:task'
ophio-intake admin --data /srv/ophio-intake get REPORT_ID
ophio-intake admin --data /srv/ophio-intake admit REPORT_ID
ophio-intake admin --data /srv/ophio-intake claim worker-one --lease-ms 300000
ophio-intake admin --data /srv/ophio-intake claim-check worker-one --lease-ms 2700000
ophio-intake admin --data /srv/ophio-intake status REPORT_ID '{"state":"reproduced"}'
ophio-intake admin --data /srv/ophio-intake ask REPORT_ID '{"template":"happened_again"}'
ophio-intake admin --data /srv/ophio-intake close-question REPORT_ID QUESTION_ID
ophio-intake admin --data /srv/ophio-intake known known-issue.json
ophio-intake admin --data /srv/ophio-intake publish release.json
ophio-intake admin --data /srv/ophio-intake widen BUILD early
ophio-intake admin --data /srv/ophio-intake halt BUILD
ophio-intake admin --data /srv/ophio-intake complete BUILD
```

`known-issue.json` is a `KnownIssue`, for example:

```json
{"signature":"empty@/work/:task","affected_builds":["build-1"],"fixed_in":"build-2"}
```

An empty affected-build list matches all builds. A matching submission gets
`Known { fixed_in }` immediately. New reports with a signature matching an
open report join that duplicate group, except a report carrying a test always
starts its own group with itself as the first report. Each submitted test thus
keeps its own inbox item, check and Look into it choice, even with a matching
signature. Later reports without tests may join a test submission's group.
Grouping does not expose identities or content to other reporters.

`release.json` is the protocol's `ReleaseEntry`, for example:

```json
{"build":"build-2","published_at":1792000000000,"ring":"owner","state":"rolling","fixes":["empty@/work/:task"]}
```

Publication moves matching open reports to `Fixed`; widening to `everyone`
or completing moves them to `Shipped`. Rings cannot narrow. A halted release
stays in the feed and cannot complete. Halting does not erase report history.
This feed describes releases; the intake never builds, signs, installs or
ships binaries. `admin command FILE` accepts any typed `Command` as JSON.
Local `list`, `get` and similar commands print private data; use a private
terminal and protect any redirected output.

## The owner's report inbox

The owner's own companion can read the private API and show each new problem
report as one Needs-you item on their devices. Add this to that companion's
`config.toml` only, never to a companion the token shouldn't reach:

```toml
[report_inbox]
intake_url = "http://127.0.0.1:8071"
intake_token_file = "/home/you/.local/share/ophio-intake/maintainer.token"
# poll_seconds = 30
```

- **One item per problem.** A report that joins an open duplicate group
  updates its group's item ("Reported 3 times") and doesn't alert again. A
  repeat brings back an item the owner put away, quietly. A test submission
  always has its own group and item, never hidden in another report's group.
- **The backlog is quiet.** On the first sync every report already received
  becomes a quiet item; only reports after that alert, and only if Problem
  reports alerts are on.
- **Words are data.** The item's title is fixed text with the report's
  reference. The reporter's sentence, screen, build and phone show in the app
  as one plain line each, with control and direction characters removed, and
  never reach an agent, a prompt or the notification itself.
- **A submitted test shows as lines too.** When a report carries a test, the
  item names its path and kind and the program and model that say they wrote
  it, then what the fixer's check showed at their build and on main ("not
  checked yet" until then). The wording describes only what was observed:
  "reports an assertion failure", "reports a failure without an assertion",
  "passes", "stops abnormally (crash or early exit)", "its output couldn't be
  read as a test report", or "hit the runner's limits", as applicable. The
  test controls its own output and exit, so a reported assertion is not proof
  of a bug. When it reports an assertion failure at their build and passes
  on main, the item says "Fails at their build and passes on main: it may
  already be fixed there." A failure site is labelled as a hint. The share
  count is distinct reporter keys with other open reports at that site,
  outside this item's whole group: multiple reports from one key count once.
  It says "1 other reporter's open report fails here" or "2 other reporters
  have open reports that fail here", not a count of reports or verified
  people. On a computer that advertises
  `report_inbox_code_v1`, See test and Their suggested fix (when they sent
  one) open the full text on demand, in monospace, labelled as written by
  their model and not checked. Ophio never applies the fix. Control and
  direction characters are removed before display. The code is only for the
  person: it is never in a notification, never in an agent's or a linked
  chat's view, and never in a prompt. Inbox items are also withheld from
  event subscriptions on in-run agent connections, including the owner socket.
- **It follows the report.** Its status shows on the item; when the report
  closes the item goes. Putting it away (Done) only closes the owner's item,
  never the report.
- **The owner answers.** On the item the owner can mark the problem
  Reproduced, Couldn't reproduce, Fixed in this build (the build their
  computer runs) or Declined with a reason. The verdict goes to every open
  report in the group, so everyone who sent it sees the same status in Your
  reports; Reproduced and Fixed also alert them once. A withdrawn or closed
  report is left alone. Only the person answers, from a paired device or at
  the computer with the admin grant; a linked chat can't. If the intake is
  out of reach the item says "not yet sent to reporters" and the companion
  retries; a later verdict replaces one not yet sent. Declining closes the
  reports, so the item goes.
- **The fixer waits for the owner.** On a computer whose report fixer runs
  (`[pipeline]` in the same `config.toml`), the item also offers Look into
  it. Only that admits the group's first report at the intake, and the fixer
  claims nothing else. The item says "You chose it for the fixer; not yet
  sent" until the intake has it, then "Ophio is looking into it". A report
  that closed first is never admitted. Only the person chooses, as with
  verdicts. A check's `agent_block` also hides the button and makes the
  command refuse: `unreducible` shows "Look into it can't use it: its code
  can't be read safely enough to show an agent." `long_literal` shows "Look
  into it can't use it: it has a string longer than 256 bytes, which no agent
  is shown." The person can still use See test to read its text. Without a
  fixer the button doesn't show and the command is refused.

The companion reads the token file once at start and keeps the token in
memory. A missing or unreadable file leaves the inbox off with a warning
that names neither the file nor its contents.

## Checking it end to end

From the repository root, run the companion against in-process reporter and
maintainer listeners with temporary storage:

```sh
env -u WAYLAND_DISPLAY -u DISPLAY cargo test --locked -p services --test reports_intake
```

This checks signed, reviewed submissions, retries, status and one-time alerts,
questions and answers, withdrawal, intake closure and reopening, and release
publication, widening and halt. Polls and retries use the service's test
trigger without waiting for timers. The intake's `close` switch pauses new
reports; the independent pipeline `pause` switch does not.

For the phone's direct path, install the locked client dependencies once,
then run the driver:

```sh
cd client && npm ci && cd ..
node client/scripts/intake-e2e.mjs
```

The driver builds `ophio-intake` offline, starts two temporary loopback
listeners, runs the real phone report service with WebCrypto keys and fetch,
and stops the process and removes its data even when a check fails. It checks
submission, status, a question and answer, withdrawal and reporter CORS
preflights. It never uses a configured intake or existing credentials.
Cargo dependencies must already be cached. The same driver is a CI stage:

```sh
bash scripts/ci.sh intake-e2e
```

The Vitest file is skipped in ordinary `npm test` runs. To use a separately
started disposable intake, set `OPHIO_INTAKE_E2E_URL`,
`OPHIO_INTAKE_E2E_MAINTAINER_URL`, and `OPHIO_INTAKE_E2E_TOKEN` in its
environment, then run `cd client && npm test -- src/diagnostics/intakeClient.e2e.test.ts`.
Keep the token out of shell history and output.

## Backups and retention

SQLite uses bundled SQLite, WAL mode, a busy timeout, versioned migrations,
and secure deletion of freed database cells. Preserve `intake.sqlite`,
`config.json` and the token in protected, encrypted backups. Use SQLite's
online backup API or `.backup` command for a consistent database snapshot.
Do not copy just the live main file while writers are active; committed data
may still be in `-wal`. For a filesystem backup, stop the intake and any local
admin writers, then preserve the database with its WAL/SHM siblings. Test
restores in a private directory with a compatible binary. A newer schema is
refused by older binaries.

At startup and hourly, the intake deletes bundles of closed reports after
`retention_days` (90 by default), including free-text question answers.
Closed means shipped, declined or withdrawn. Open reports remain available.
`admin sweep` runs the same retention operation immediately as an audited
maintainer command. Its `Swept` result counts reports whose stored content
was removed. A test submission's check goes with its bundle. Withdrawal
removes the bundle and any check, questions, signature and duplicate link immediately,
keeping an ownership/status tombstone so retries cannot restore the content.
A withdrawn report cannot be reopened through the maintainer API.

Deletion is logical within the current database. Old pages may remain in a
live WAL until checkpointed, and backups may retain earlier content. Plan
backup expiry and encrypted-volume disposal to match the privacy policy;
secure deletion is not a promise of forensic erasure from all storage.
Counters, structured histories and audit metadata remain after retention.

## Stored data and logging

Bundles are normalized JSON after validation. The server checks schema,
character limits, identifiers and control characters, trail/frame counts,
base64 size, image format magic, encoded/declared image dimensions and a
submitted test's form.
Pictures must be JPEG, PNG or WebP, at most 8192 pixels on either side and
32 million pixels in total. It reads image headers without decompressing
pixels. The device remains responsible for redaction and consent before
submission; validation cannot prove that arbitrary text was redacted.

The intake never collects device files, desktop images, credentials, chat
history or project content on its own. A submitted test and suggested fix
are code the reporter's model wrote; the intake never runs them. The person's sentence and explicitly
consented extras can contain private text; consented screenshots show the
app's own screen. Protect bundles as sensitive data. It never stores bearer
tokens in SQLite, raw socket IPs, raw public keys, or authentication headers
in logs. Rate buckets use hashes of IPs; these are pseudonyms, not anonymous
against guessing. Replay rows contain canonical signatures and expiry times.

Logs contain ids, sizes, key fingerprints and fixed verdicts, never bundle
contents, words, question answers or screenshots. Audit rows carry timestamps,
operation names and applicable ids/fingerprints, not command bodies. Do not
add proxy body logging, SQL parameter tracing, or error dumps containing
report data.
