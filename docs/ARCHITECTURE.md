# Architecture

This repository contains two products that ship together:

- **The phone client** (`client/`): an Android app built with Tauri 2 and
  React/TypeScript. It pairs with one or more computers and lets one person
  talk to coding agents, watch and take over the desktop, point at the screen
  and ask, and edit agent Markdown files.
- **The host companion** (`crates/`): a Rust service for Linux; its Windows
  build is not yet qualified (see [SUPPORT.md](../SUPPORT.md)). It
  owns every durable fact (tasks, runs, approvals, leases, the event journal)
  and drives native tools — Claude Code, Codex, the desktop session, terminals.

The phone never talks to agents or the desktop directly. Everything goes
through the companion, which records it first.

## Principles

1. **The host is authoritative.** The phone is authoritative only for unsent
   drafts. Every effectful request is a command with a client-generated id; the
   companion records the id before acting, answers retries of the same id with
   the original receipt, and reports later effects as journal events.
2. **Truthful state.** A receipt means *admitted*, not *done*. Command stages
   (`host_accepted → native_acknowledged → observed`, or `uncertain →
   reconciliation_required`) are shown as they are. Unknown is a valid state and
   is never rounded up to success.
3. **Stop is never queued behind anything.** Control/events, media, terminal,
   transfer and speech each use their own authenticated connection.
4. **Least authority.** A paired device holds explicit grants. Desktop input
   needs a lease bound to one target, one lease generation and one boot epoch.
   Library access is confined to registered roots. Command status and replayed
   command results require current grants for the original command as well as
   ownership (or management access); removing access withholds stored results
   and receipts too. A retained command whose status is withheld returns a
   permission-denied query error, not an empty status: recovery must keep its
   original command ID and unknown outcome until access returns. One denied
   status lookup does not block reconciliation of other commands.
5. **Native first.** Harness adapters drive the tools' own supported
   interfaces (Claude Code stream-JSON, Codex app-server) using the person's own
   signed-in accounts. The companion never extracts subscription tokens.

Chat, annotation and transfer-composer drafts distinguish an unread local store
from an empty one. A failed initial read blocks replacement writes; later edits
or the visible Retry button read again and merge work created during the outage.
Chat keeps competing drafts in Saved drafts, annotations retain both draft IDs,
and the transfer composer applies only fields edited during the outage. Competing
issue drafts for the same project are kept under
`ui.<profile>.issue-drafts.recovered` before replacement; other projects' issue
drafts merge into the recovered state. If that copy fails, the original stays.
Competing transfer text or file metadata is copied to
`transfer-composer.<profile>.recovered`
before replacement. A reread advances the draft revision beyond uploads started
while storage was unreadable, so their completion cannot clear restored content
or a restored composer mode. Later submissions use the recovered revision.

Malformed JSON follows Library's recovery rule: keep the exact bytes under
`<key>.unreadable` before replacing the original. Numbered suffixes preserve
distinct recovery copies. If reading or making the copy fails, the warning stays
visible and new work remains in memory. A successful read alone does not clear a
save warning: the merged write must succeed. Recovery copies stay on the device;
Settings' allowlisted device export does not include these raw copies. These
rules live in shared TypeScript and apply to Android and iPhone alike; they do
not establish physical-device qualification.

Accepted follow-ups enter the conversation in the same transaction as their
command receipt, with their text, annotations, message reference, original run,
and `host_accepted` delivery state. Execution updates that row through delivery
events; it never moves the message to a later run or appends the text again.
Before native dispatch, Stop cancels the message and other failures mark it
undelivered. Restart settles messages known never to have been dispatched,
without replaying them. Once dispatch has started, an uncertain outcome stays
uncertain until the adapter supplies evidence. Retrying a command id returns its
original receipt and leaves exactly one conversation message.
Chat revocation excludes undispatched messages from native-delivery uncertainty;
their send workers fail authorization and leave the saved messages recoverable.

A chat whose reply was cut off (its run's result is unknown, for example because
Øphio restarted mid-turn) carries on when the person writes again, but only when
the computer knows that the agent and everything it started inside the run's
container have stopped. It records
that as `nothing_running` on the run, in two cases. When the agent's exit was
reported and the adapter keeps every process the run started together
(`Containment::Complete`: a Linux cgroup or a Windows job object), that exit
already says they have all ended. After a restart, the run's own Linux cgroup,
recorded when the run started, must be empty or gone from a cgroup hierarchy
that is still there, or the computer must have rebooted since (a new boot id).
For a chat, that proof also settles recorded local helpers as stopped, with an
observed end time, and resolves their attention items. In-process helpers end
with the agent; separate-process helpers are settled only when the recorded
adapter containment is complete. Starting a new session or moving a local shell
command into the background does not escape a Linux cgroup. Unknown helper
lifetimes (including the helpers Codex currently reports) are never inferred
ended. A run with unconfirmed cleanup at startup, a helper still active or of
unknown state, or only an unconfirmed process group to go on keeps the review,
and so does every Task. Without a restart, an adapter's confirmed exit settles
local helpers by the same rule only with complete containment and no outcome
entries still active or unknown. The reply is an ordinary
`resume_task` with the person's new words and no mode change. As soon as the
cut-off chat can carry on, its outcome-decision attention items are resolved and
its receipt no longer asks the person to mark it failed or completed. A single
informational reply notification says “Reply cut off. Write to carry on.” It
uses the existing `Completed` attention kind with a headline and the Chat's
`Reply` notification settings; this never changes the run's unknown result.
It opens Chat, stays out of Home's decisions, and is acknowledged when the Chat
is seen. A per-run `cut_off_told` record commits with the confirmation and item,
so a restart cannot raise it again, even after acknowledgement. Already
carried-on runs are not notified. No success or human verdict is invented.
In the same write as the person's next message,
the cut-off run is settled as `interrupted` with `carried_on` (who and when; its
result stays unknown and is never recorded as completed), and it lets go of the
project's writer. Its old receipt says it was cut off and carried on in the next
reply; changed files, steps and reports remain available. The
interrupted prompt is never sent again. The phone offers this only on a computer
that advertises `chat_carry_on_v1`; otherwise, and whenever the computer can't
confirm the run's container is empty, Mark failed and Mark completed stay the
way on. Stored phone commands whose own outcome is unknown are not touched.
An empty container is the evidence accepted here, the same evidence an
ordinary next turn follows: work the agent handed to something outside it,
such as a systemd unit started with `systemd-run --user`, may still be running
(see the containment section below), and the review this replaces could not see
it either. So the phone says only that the agent and what it started have
stopped, and the carry-on turn tells the agent that its last reply was cut off
and that anything it started outside the session may still be running.

Streamed reply text is not a replayable live event. Masked deltas reach the
phone immediately and stay in separate per-message buffers within each run.
A background worker checkpoints every message's unsaved text together about once
a second; 8 KiB of unsaved text across the run also triggers a checkpoint. These
writes append only new text and update small per-message saved-byte markers,
rather than rewriting the whole reply. Disk work for periodic and size-triggered
checkpoints runs off the async executor. Failed transactions leave the memory
buffer intact; committed byte markers make retries safe even if publication fails.
A hard crash can lose roughly the last second of the reply, or more if the disk
cannot save it. Graceful shutdown saves the remaining tail.

An admitted person message or a conversation handoff saves every pending tail
and turns pending words into `AgentMessage { partial: true }`
timeline items in the same transaction. Helper activity, usage, warnings and
status events do not split an ongoing reply. Activity events do not identify
whether they came from the main agent, so they are not message boundaries.
Different message keys can stream concurrently and never end each other. A final
native message saves only its own tail and normally replaces its own pending text
with one whole message; if a genuine interruption already recorded a prefix, only
the remaining words are added. Each key has independent saved-byte tracking, so
finishing one leaves the other keys intact. A key without a final message stays
pending until interruption or run end. A checkpoint failure for one run does
not prevent the other runs from saving that tick.
Run settlement saves and drains the tail; startup drains the saved checkpoints.
Both clear pending text atomically, including when recovery still needs review.
A saved cut-off reply can therefore appear after helper steps that arrived while
it was being written. Long partial messages are split into frame-sized items
without dropping text.

Timeline sequence numbers remain the identity for replay and paging, so a second
restart does not append the words again. The phone replaces the matching live
copy with the journaled partial and uses the existing quiet Cut off divider;
partial text does not claim that a step succeeded. Secret masking and existing
native-event size limits still apply before persistence or live publication.

Ordinary continuation resumes the native Claude Code or Codex session, rather
than injecting the transcript again. When Ophio itself captures chat context for
a separate task, it includes partial replies labelled as partial, within that
snapshot's existing size and history limits. This preserves what Ophio received;
it does not recover text already lost by an older version or text an agent never
sent, nor does it guarantee how much an abruptly killed native agent saved in
its own session.

Helper answers waiting for delivery are separate from admitted messages. Stop
removes those queued answers and outstanding consultations in its admission
transaction; each helper's original answer stays in that helper's task. A delivery
worker checks the stored follow-up's command id again when admitting a new turn,
a queue continuation, or a message to a running agent. Copies already read into
a delivery batch therefore cannot revive cancelled work. Retry and cleanup use
the same identity check so an old worker cannot recreate a cancelled answer or
delete a replacement wake.

## Crate layering

```
protocol   wire contract (serde + JSON Schema + generated TypeScript)
   ↑
domain     pure state machines: task/run lifecycle, approvals, leases,
           queue, reconciliation (no I/O)
   ↑
ports      traits: HarnessAdapter, DesktopBackend, TerminalHost,
           SpeechEngine, LibraryFs, MediaEncoder, Clock
   ↑
store      SQLite (WAL, synchronous=FULL): journal, projections, command
           dedupe, devices, artifacts
services   admission, dispatch, control arbitration, library, transfers,
           notifications, readiness
   ↑
harness-claude, harness-codex        implement HarnessAdapter
containment                          one process container per agent run
platform-linux, platform-windows,    implement DesktopBackend
  platform-macos
platform-desktop                     desktop session shared by Windows and Mac
terminal                             implements TerminalHost
media                                tile-delta encoder and flow control
channels                             Telegram and Discord
computer-mcp                         local MCP computer-use facade
   ↑
companion  binary: configuration, TLS listener, connection handlers,
           pairing CLI, service install
```

`domain` has no async and no I/O so its rules can be tested exhaustively.
`services` reaches native adapters only through `ports` traits and uses `store`
directly; adapters are wired in `companion`.

Outside-agent observations refresh on demand at most every ten seconds. A
successful scan reconciles its first 256 session records against an index of
saved `(harness, native_session)` identities, including finished managed runs.
This lookup never decodes run payloads or reported diffs; empty record lists
and failed scans perform no identity lookup. SQLite builds the additive index
once for existing databases and maintains it when any host version writes runs.
Failed scans retain the last observations with Stop disabled until a refresh.

Artifact blobs are shared by content hash. Once an upload completes, its
artifact metadata owns the blob; the completed transfer is history and does
not retain bytes by itself. Deleting the last artifact removes its blob unless
another unfinished artifact upload or unexpired download still retains it.
Publication holds the blob read lock until metadata is committed, excluding
collection during that interval. Startup and hourly transfer collection reclaim
unreferenced blobs, including those stranded by older completed uploads.

Telegram and Discord notification delivery checks the current linked identity,
grants and event eligibility before querying a snapshot. Timeline and run updates,
nonterminal or delegated task updates, and task or approval events for other
projects do not load retained history. Eligible notices still use a fresh snapshot
to check task visibility and current approval or attention state; attention events
carry no project id, so their task's project is checked there. Reconnecting channels
still repair approval notices from a snapshot.

## Shown artifact expiry

The hourly expiry sweep keeps the existing seven-day retention and source
protection rules (see [collaboration](collaboration.md)). Within its database
write, it loads saved prompt and queued-request references once, lazily when
an artifact is otherwise eligible to expire. The same queue scan collects tasks
with pending items: a stopped task keeps its pictures while its continuation
waits in Next up, including a paused queue, a permission wait or a blocked
dependency. Each candidate checks the sent artifact IDs and all its attached
task IDs. Started queue history does not protect a finished task. The sets are
discarded after the transaction so later sweeps see added or removed references
and pending work. This avoids deserializing every saved prompt again for each
old screenshot while holding the shared database lock.

The expiry scaling regression counts executed prompt/queue collection queries
through the store's test-only SQL tracing feature. It covers 1,000 saved 8 KiB
prompts, over 1,000 screenshots, reference protection and a later sweep after
references are removed.

## Connections and authentication

All connections are WebSockets over TLS to the companion's self-signed
certificate. The phone pins the certificate's SHA-256 from the pairing QR code.
When no address presents that certificate, as when another device answers at
the computer's address on an unfamiliar network, the phone sends it nothing,
says so, and keeps reconnecting with the backoff described below. It returns to
normal once the computer answers again; forgetting and pairing again is left to
the person, for a computer whose certificate really changed.

Pairing: the host shows a QR code (`PairingOffer`) with its addresses,
certificate fingerprint and a one-time secret. The phone generates an Ed25519
device key (wrapped by an Android Keystore key) and sends `PairRequest` with the
secret and its public key. The companion holds that request open until the
local owner answers it at the computer: both screens show `pairing_code`, six
digits derived from the certificate fingerprint, the secret and the device key,
and only a confirmed request enrolls the device and returns its id and grants.
One request waits at a time. Refusing ends the window; closing the request
withdraws it.

Every connection then authenticates independently:

```
client → hello { device_id, client_nonce, channel }
server → challenge { host_id, boot_epoch, server_nonce }
client → authenticate { signature }
server → authenticated { grants, capabilities, journal_head }
```

The signature covers `auth_transcript(...)`: host id, device id, connection
kind, both nonces and a TLS exporter value (`EXPORTER-device-auth-v1`), so a
signature cannot be replayed on another TLS session or connection kind.

| Connection | Carries |
|---|---|
| control | commands, receipts, queries, journal events, live text, human input and lease renewals |
| media | tile-delta video frames and acks |
| terminal | PTY bytes (binary) and attach/resize frames |
| transfer | resumable upload/download chunks |
| speech | PCM audio up, revisable transcript segments down |

The phone pings on the control connection every 15 s and the companion answers
each ping at once. The companion closes a control connection that has said
nothing for 30 s. The phone treats a computer that has sent nothing for 45 s as
unreachable, even while the connection still looks open (a computer that is
asleep or frozen). It counts bytes rather than whole messages, so a large
message arriving slowly keeps a working link, and it keeps counting while the
phone itself sleeps, so on waking it finds such a computer within seconds.
It then shows the computer as
offline and reconnects with backoff, starting near 4 s and doubling to at most
30 s. Until the computer answers again, a retry that times out is the same
silence, so the phone keeps saying the computer is unreachable and when it was
last heard.

## Journal

The companion appends every durable fact to a journal with a monotonic
sequence. A client subscribes with the last sequence it applied; the server
replays from there, then sends `caught_up` and live events. If the requested
position was compacted away, or an event after it is too large for one frame,
the server sends `journal_gap` and the client reloads a snapshot and shows the
gap. Streaming agent text is sent as
non-durable `live` events and superseded by the journaled final message.

The phone shows a replay in one step. It holds what arrives between its
subscribe and `caught_up` and applies it together, so after time away a
project's queue jumps straight to where it is rather than counting down
through every state in between. Coming back to the front, it holds what the
link delivers in a burst until the computer answers a ping sent then. Neither
waits more than 3 s at a time: a longer replay shows how far it has come, and
an unanswered ping is let go.

Replay reduces journal and live effects in wire order, collecting timeline
items per task and merging each conversation once per batch. Delivery updates
still apply to loaded messages and to messages fetched on earlier pages.
Outside conversations being read, the phone keeps at most eight recent
timelines, each with its newest 80 items. Trimming sets `hasMore`; the first
retained item stays the cursor for reading older history without a hole.
The conversation screen retains its pages until it unmounts. In-flight history
queries retain their timeline too, so even a large burst of events arriving
before an initial query's answer cannot be lost to trimming or eviction.
These readers are local to the session and are never saved in the offline cache.
`node client/scripts/replay-benchmark.mjs` measures projection-only replay on
the local Node runtime; item-visit regressions run with the client tests.

A snapshot fits one frame however long the history. It carries everything
live (unfinished tasks and their runs, pending approvals, Next up and whatever
needs the person), then finished tasks newest first while they fit, and says
where the tasks it left out begin. The phone reads those a page at a time
when the person reaches the end of a list. Runs in a snapshot or page leave
out the diffs their agents reported; a receipt reads its run whole. A long
task carries its latest runs; its conversation names the earlier ones, which
are read by id with their approvals. A pending approval too large to share a
snapshot, page or run with the rest is named instead; the phone reads each one
alone before it uses the reply. Chat connectors read the snapshot inside the
companion, not through a frame, so theirs carries every record.

Task lookups read only that task's indexed runs, approvals and attention.
History pages seek within the project's `(updated_at, task_id)` order and
fetch at most the requested limit plus one task. Admission enumerates live
records through partial indexes; it does not construct finished-task history.
It still reserves room for pending approvals' dismissed notices and Next up.
Phone snapshots use the same live discovery, then seek tasks across projects
in `(updated_at, task_id)` order, loading related records one task at a time
until the byte budget is full. The first excluded task remains the inclusive
history cursor. Indexed seeks identify projects below that cursor without
loading their task payloads, including tasks whose project no longer exists.
Adding older tasks beyond that cursor does not add related-record reads.
Next up refreshes read only the run summaries named by pending items'
dependencies, once per distinct run in each queue. Empty queues and items
without dependencies read no run history. Missing prerequisites and runs with
active or unknown helpers keep their existing blocking behavior.

SQLite keeps a separate run summary without reported diffs, maintained by
triggers in the same transaction as each full run. The original run remains
available for receipt reads. An indexed set of unresolved notices without a
stored task preserves their admission weight without scanning finished tasks'
notices. The first open backfills these additive projections transactionally;
subsequent opens reuse them. Triggers keep them current for older hosts that
write the original document table. This adds summary storage and write-time
work in exchange for avoiding full historical payloads during small reads.
Internal chat snapshots still read complete records. No phone protocol or
native platform change is needed; Android and iPhone share the companion fix.

## Desktop control

Watching is read-only by default. Managed input requires a lease:

- one managed input owner per target (a human device or one computer-use run);
- every input action carries lease id, generation and the geometry revision the
  phone computed coordinates against — any mismatch is rejected, never
  reinterpreted;
- human leases are renewed only while the control view is foregrounded; the
  input worker enforces the deadline on its own monotonic clock and releases
  every held key/button on revoke or expiry;
- a computer-use run's lease is renewed only while its supervising device
  watches the target; once it lapses, the run's next input action waits up to
  10 minutes for the person to resume it, then is refused with a fresh
  observation required;
- a companion restart changes the boot epoch and voids all leases.

Model observation (screenshots delivered to an agent) is a separate lifecycle
from input. A *private step* stops agent input and managed screenshots, reports
which routes it could and could not cover, and only then hands control to the
person.

Physical local input is outside this arbitration and always works.

## Live view codec

The companion captures a target, splits it into tiles, and sends only changed
tiles as JPEG with latest-frame-wins flow control (a small window of unacked
frames; changes coalesce while waiting). The client composes tiles onto a
canvas with `createImageBitmap`. This keeps latency bounded on slow networks
without requiring WebRTC in the Android WebView. The binary layout is
documented in `crates/protocol/src/media.rs`.

## Harness adapters

- **Claude Code**: `claude -p --input-format stream-json --output-format
  stream-json` with session resume; tool approvals are routed to the phone
  through a permission-prompt MCP tool served by the companion.
- **Codex**: `codex app-server` JSON-RPC (threads, turns, interrupts, approval
  requests). User-input requests expose each question and its options on the
  phone. Optional opaque question IDs travel with questions and answers, so
  repeated wording remains unambiguous. Codex assigns these IDs by position
  within the request and maps them privately to native IDs. Secret masking
  changes visible text, never these opaque IDs. The adapter validates complete
  answers and returns them under native IDs; single-question text and explicitly keyed
  JSON replies remain supported for older pending cards. Adapters without IDs
  and older phones continue matching exact question text only when it identifies
  one question. Answers arriving after the carry-on timeout include the original
  question order, IDs and headers, preserving their associations in the follow-up.
  Requests made through Ophio's bridge (`request_access` and the other Ophio
  tools) that carry on also stay open when the agent stops waiting early: when
  its bridge connection closes or its turn ends without a person's Stop, the
  request is marked carried on instead of expired, and a later answer is the
  task's next message. An answer admitted as the agent stops waiting, which
  no longer reaches the wait, takes the same path. Access requests carry no
  deadline of their own, so an Allow is checked against the access as it is
  when admitted: if it would turn on more than the card listed, or lift every
  limit without the card's warning, it is refused and the card withdrawn.
  A later Allow on a command, file change, network or tool request the agent
  made (never an access request, a question, Ophio's own gate or its own
  question about starting an app) leaves a one-use pass for 30 minutes,
  stored per task and request, so the agent's identical retry in that task
  is allowed without a second card. The pass is used up once, never renews
  from the Allow it gave, and is cancelled by a Deny of the same request in
  that task, by the person's Stop and by archiving. A private (secret)
  question never carries on: its answer may only reach the live native
  question, so the ten-minute timer, the turn's end and a closed bridge
  connection expire it instead.
  The shared web form and Rust adapter work for Android and iPhone;
  no platform-specific command changes are needed.

Each adapter reports a truthful capability table (steering, interrupt, native
approvals, child agents, resume). Unsupported features are shown as
unsupported.

Adapters read a run's changed files and checks from native tool results,
never from the agent's prose. Shared code in `ports::checks` decides which
commands are checks, whether a command line's exit status is the check's own,
and whether the run was verified after its last change. When a task's next
run ends, the run before it records whether it needed a follow-up. See
[run-results.md](run-results.md).

Each run starts inside a container of its own (`crates/containment`), and Stop,
the end of the run and the companion's shutdown end everything in it before the
run's exit is reported, including work the agent moved into the background. On
Linux the container is a cgroup made inside the companion's own cgroup; nothing
the agent starts can leave it with `setsid` or a double fork. Where no cgroup
can be made there (cgroup v1, a kernel older than 5.14, or a cgroup the
companion doesn't own), a run keeps only its process group and the Stop receipt
says what may still be running. On Windows the agent joins a job that allows no
breakaway before any of its code runs. Work the agent hands to another part of
the system to start, such as a systemd unit, a scheduled task or a service, is
outside the container on both, so a run whose container is empty may still
have left such work running; a cut-off chat that carries on says so to the
agent.

A limited chat also runs in a sandbox (`crates/sandbox`: bubblewrap on Linux,
the Mac's own sandbox on a Mac; Windows has none), with its own copy of the
agent's settings folder in the conversation's `state/native`. Only the
provider credential file is shared to write. The person's instructions and
skills come from a short allowlist (`crates/sandbox/src/setup.rs`) and are
shared read-only. Each one is resolved through at most one link, refused if it
reaches anything private or broad, checked without reading its contents and
held open while the run lasts. On Linux the sandbox's start-up step checks
each mount is still the same file or folder before the agent starts. A run
that can't change files skips skills whose front matter says they do, and
none is shared that the run is told to leave out (Disable in Øphio), since the
agent finds what is shared at the copy's path. See
[agent-files.md](agent-files.md#in-a-limited-chat).

Files attached to a chat message (`crates/services/src/attached.rs`) arrive as
uploaded artifacts named in the message's annotation bundle. Only the device
that uploaded a file may send it, administrators included, and nothing an agent
showed counts as uploaded. Admission replaces each file's name and media type
with the cleaned ones, so the stored command, the prompt and every device's
timeline carry the computer's name. Before the run starts the computer puts a
copy of each in `<data>/attached/<task_id>/`: a folder made 0700, outside every
project, with each file created new at 0400, never through a link, under a name
the computer derives from the display name (one component, no instruction file
names, numbered on collision), and made read-only again under that name once
the temporary name is removed (on Windows the mark belongs to the file). Each
copy made is recorded as it is made, so sending again after a failure reuses
it; a file of the message that can't be placed refuses the message, while an
earlier one that can't be put back is listed to the agent as unavailable. The
folder is made at every run start and reaches the run through
`RunSpec.attached`: Claude Code gets it with `--add-dir`, and in the Linux
sandbox it is one more read-only path, so a limited chat sees only its own
conversation's folder. A run that couldn't get its folder is marked, and files
sent to it while it runs are refused. The prompt names each file as quoted
data. Limits are in `protocol::annotation` (500 files and 1 GB per
conversation; records are found by their `{task_id}:` key prefix); an hourly
sweep removes a conversation's copies and records a week after it is archived,
and the originals unless another conversation's file, a message picture, an
agent's shown item or a waiting request still uses them. See
[chat-files.md](chat-files.md).

Continuation admission preserves the last native session and its original mode
in the durable run intent. If a launch fails before reporting a native session,
the next retry inherits that intent, including a pending Plan-to-Work transition.
An acknowledged session supersedes the intent; sessions never cross harnesses.
Attempts that fail before session start keep their native session and
acknowledgement fields empty.

Chat agent switching is separately gated by `chat_agent_switch_v1`. The
agent, model and effort belong to the next message, not the project's defaults
or structured Tasks; a queued message retains its choices. Cross-agent
messages wait for a new turn rather than steering the active agent. Before
launch, the host persists the handover boundary with the run, so recovery
cannot accidentally send the turn to the prior agent. A destination can receive
only bounded recent user messages and completed replies (2,000 characters per
message, 12,000 overall). This transcript is information, not transferred
native memory or authority. On switch-back, resume that agent's prior native
session with missed recent context only when safe; otherwise start fresh with
recent context. Destination launch still performs its normal policy, sandbox
and authorization checks; approvals, grants, pending requests and answers do
not cross agents. See [Choosing an agent for the next Chat message](chat-agent-switching.md).

## Platform backends

| Host | Capture | Input | Terminal |
|---|---|---|---|
| Linux, wlroots/Hyprland | `ext-image-copy-capture` or `wlr-screencopy` | virtual pointer / keyboard protocols | PTY |
| Linux, GNOME/KDE | XDG desktop portal ScreenCast + RemoteDesktop (PipeWire) | portal | PTY |
| Linux, X11 | GetImage (no MIT-SHM) | XTest | PTY |
| Windows 11 (partly qualified) | Windows Graphics Capture, DXGI fallback | SendInput | ConPTY |

Each backend reports availability per capability (`available`, `unsupported`,
`permission_missing`, …) instead of failing silently.

## Client

The client is a Tauri 2 app. Networking, TLS pinning and device keys run in the
Rust side of the app; the React UI receives events over Tauri IPC and media
frames, unchanged, over a raw IPC channel (never base64), and the WebView
decodes the JPEG tiles. A small
Kotlin plugin provides Android Keystore wrapping, share targets and
notifications.

Each tab keeps its navigation stack in `UiStore`. Library breadcrumbs and
links back to an open settings screen use `up`: return to the nearest matching
route in the active tab, or push it when absent. Returning removes the screens
above that destination, so Back continues toward the parent instead of reopening
the file just left. A cross-tab return marker survives only while its destination
remains in the stack. This navigation is shared by the phone clients.

File links in agent replies open in Library when they name a file inside the
task's project root. The shared Markdown renderer preserves Windows drive
paths with forward slashes or backslashes, including Markdown's encoded
backslashes, and numeric line citations. Other addresses keep the renderer's
normal protocol filter. A Markdown destination extension preserves Windows
separators before punctuation such as `.config.md` and `_report.md` can be
unescaped. It covers inline and reference links while leaving code, labels,
titles and other destinations alone. The file handler checks the project root
and removes line citations before requesting the file; clicks always cancel
browser navigation. This behavior is shared by Android and iPhone.

Library saves drafts and expendable read caches together in device storage.
If its initial read fails, that store keeps later changes only
in memory and blocks every storage write, including automatic cache refreshes,
so unread drafts remain recoverable. Its warning stays visible and asks the
person to copy new edits elsewhere before restarting the app to retry loading.
Write failures after a successful load can still recover when storage becomes
available; cached bodies are evicted first when quota is exhausted.

Library and transfer history handle damaged JSON separately from read failures.
Before allowing new saves, they preserve the exact unreadable string under
`library.v1.<profile>.unreadable` or `transfers.v1.<profile>.unreadable` in the
same device storage. Distinct later corruptions use `.unreadable.1`,
`.unreadable.2`, and so on; reopening the same corrupt value reuses its copy.
Recovery copies are never automatically deleted or replaced. A warning explains
that earlier entries could not be loaded and new entries can be saved again.
This does not repair the damaged entries or recover old download identifiers;
the raw copies remain available for diagnosis/recovery through a storage
inspector, without clearing the rest of the app's data.

If the recovery copy cannot be read or written (including a full device), the
original value stays protected and the warning explains that new work is only
in memory. Free space before retrying; Library retries on restart, while
transfers retry before saving and on connection/foreground changes. A read
failure never starts an empty history: transfers reread and merge the saved
records with live jobs before saving. This recovery uses shared TypeScript on
Android and iPhone; it needs no native plugin changes.

Transfer byte callbacks keep only the latest counters for each active job.
One 250 ms timer per computer batches those counters into an immutable store
update and a history save, bounding progress-only notifications and synchronous
storage writes to four per second regardless of chunk count or concurrent jobs.
Declaration IDs and durable job changes still save immediately, including all
pending counters. Pause, failure, completion, document backgrounding and page
exit flush pending progress and clear its timer. A sudden process death can lose
the latest unflushed counters; uploads resume from the computer's
confirmed offset and downloads from the partial bytes retained on the device.
This behavior is shared by Android and iPhone without native plugin changes.

Files chosen for a chat message (`client/src/screens/talk/chatFiles.ts`) keep
their bytes in the annotation source store and their names, sizes and upload
state on the draft, so a draft with files survives a restart. Send uploads the
files that have no artifact yet, records each artifact on the draft as it
arrives, and sends the command only when all have; a retry resumes a recorded
transfer or starts a new one if the computer closed it. The picker is a plain
`<input type="file" multiple>`, shared by Android and iPhone. The phone offers
Files only when the computer advertises `chat_files_v1` and the device holds
the transfers grant.

## Versioning

`protocol::PROTOCOL_MAJOR` changes only on incompatible wire changes; peers
with different majors refuse to talk and say so. Minor additions are
advertised through `HostCapabilities.features`.

A forgotten pairing keeps its local drafts and Inbox items. Pairing the same
host again carries them forward before opening its screens, after flushing the
old interface store. Command observations remain tied to the old device key.
Migration changes storage keys and structured pairing references, never draft
text. Conflicting or unwritable values remain under the old pairing with a
recovery notice. Transfer bytes retain their source copy so a partial storage
failure cannot strand a draft that still references them.
