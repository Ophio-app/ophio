# Report worker security limits

Keep the worker disabled until these limits have been addressed for the host
and repository. Passing the local test suite does not establish live sandbox
containment or safe operation with real credentials.

## Credentials

Configuration refuses intake and provider credential files inside any runtime
mount, offline cache, executable or shim alias, mirror, or state tree. Paths
are resolved through symlinks and `..`; credentials with additional hard links
are refused. The implicit provenance key's state location is checked against
the other mounts too. Runtime paths and executable aliases come from the
sandbox crate. Keep these host-owned paths stable while the worker runs.

The scanner also watches the actual intake and provider token contents on the
host. Those values are never planted, formatted in Debug, or returned by a
match. Relay bodies, agent output and imported artifacts are checked. Encoded,
transformed or partial leaks can still evade matching; a sanitized runtime
image remains a stronger boundary than mounting host runtime trees.

## Spending and disk space

The relay now atomically charges each admitted request and reserves input and
output tokens before forwarding. Its conservative input bound is one token per
byte of the larger of the original and forwarded JSON bodies, including JSON
overhead; no model-specific tokenizer is available. Output limits are clamped
to the remaining allowance. Complete JSON and streaming responses settle to
provider usage and refund unused reservations. Interrupted responses keep the
full reservation. A provider reporting more than reserved is charged its real
usage and closes further admission. The provider must honor the output limit;
unexpected provider overruns cannot be recovered after spending.

Anonymous sandbox scratch mounts now have kernel size caps, and nested user
namespaces are disabled. Writable host binds (workspace, output, provider
state, HOME and Cargo target) share a sampled byte/entry budget. The watchdog
derives these paths from the actual mount arguments and kills the contained
process tree with a budget signal when the budget cannot be verified or is
exceeded. It also watches host-volume free space to catch unlinked open files.
The worker reserves 512 MiB of free-space headroom and 1,024 spare inodes
where the filesystem reports a fixed inode capacity.

Host binds use a watchdog because completed agent files must remain available
for trusted import and build output can exceed available tmpfs memory. This is
a floor, not a kernel quota: a burst can overshoot between scans (100 ms plus
scan time), and concurrent host writes/deletes can affect volume accounting.
Use a dedicated volume with a project quota or a disposable VM when a hard
host-disk boundary is required. Defaults and configuration are documented in
[report fix checks](report-pipeline.md#running-the-worker).

## Merge authority

The companion records every published report-fix commit in its durable store
before pushing, then records its PR number after creation. The worker currently
authors and pushes one commit per proposal. Agent tools, Through merge, merge
checks, Merge together, the merge train and the watcher refuse both `report/`
branches and PRs containing a recorded commit, including copied heads and
composed branches. Recorded PR numbers remain protected after head changes.
Commit reads are capped at 100; incomplete histories in a repository with report
fixes hold the merge. The owner's individual Merge action still requires its
open review and isolated checks on the same head.

No companion operation enables GitHub auto-merge. Both the PR sheet read and
the final isolated-CI read refuse an owner's merge while auto-merge is enabled;
the worker also refuses that CI state. Repository writers can still enable
auto-merge or merge directly outside Ophio, including between the final read
and merge. Default-branch protection must enforce owner authorization at the
forge as well. Keep auto-merge disabled for unapproved report PRs.

## Recovery and scheduling

Questions now wait durably for bounded answers and resume triage through the
untrusted reporter-data fence. A persisted deadline expires unanswered
questions. Transient read failures use persisted, bounded exponential retries;
unknown agent and mutation outcomes still stop without replay. Every stop
persists revocation cleanup for its owner attention and merge approval, and
retries that cleanup across restart and admission pauses. Intake completion
and live leases now govern claims without a growing local exclusion list.
Completed claims remain retired after lease expiry; unfinished expired claims
can be offered again.

Waiting for CI or approval occupies the single active report slot without a
deadline; new triage waits too.

Owner-ring publication writes feed metadata and provenance after a recorded
owner merge and build ancestry check. It invokes no application signer,
installer or release upload, and never widens the ring.
