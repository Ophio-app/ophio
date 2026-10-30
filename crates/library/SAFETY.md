# Filesystem adapter integration and limits

Construct `FsLibrary::new(LibraryConfig::new(data_dir, instruction_home), clock)`
with a dedicated, owner-private data directory outside every Library root.
`set_roots` validates the complete replacement before installing it. The caller
still enforces per-principal grants; this adapter enforces filesystem roots and
read-only settings. Calls run in Tokio blocking workers. One instance serializes
its mutations and root changes; use one instance per data directory.

## Paths and source fidelity

All wire paths are UTF-8, relative and slash-separated. Traversal, NUL, drive/UNC
names, alternate streams, Windows reserved names and trailing-dot/space aliases
are rejected on both platforms. Redundant `.` and separators are normalized.
Root identity is pinned with a directory handle and rechecked on requests.
Traversal uses capability directory handles and no-follow opens. All symlinks
are listed as links but refused for operations, including links within a root.
Unix multiply-linked files are refused. Device files, sockets and FIFOs are not
opened as editable documents; Unix nonblocking opens protect against a FIFO
substitution race. Listings reject non-UTF-8 entries instead of lossily aliasing
their identities. Hidden means a dotfile or Windows hidden attribute.

Reads are capped (2 MiB default), regular-file-only, valid UTF-8 and free of binary
control characters. Plain text is supported regardless of suffix; other resources
remain visible. Revisions hash exact bytes with SHA-256. Reads preserve text
including mixed line endings, and expose BOM separately. Save requests explicitly
select LF/CRLF or preserve mixed input verbatim. Unix permission bits are copied;
Windows native replacement also retains the target's replacement metadata.
Frontmatter is never parsed and rewritten. ACLs, extended attributes and ownership
beyond the native replacement behavior are not promised across platforms.

## Saves and recovery

Save first compares the base hash. It records the base and proposal privately,
flushes a same-directory temporary file, compares again, and atomically exchanges
it with the target on Linux (`renameat2(RENAME_EXCHANGE)`). Windows uses
`ReplaceFileW` with a backup. The displaced inode is retained under an opaque
`.ophio-save-*` name without a Markdown extension, and is hidden from Library
browse/search. Files an earlier version left under `.companion-` names are
hidden and recovered the same way. Linux flushes directory handles after
publication. Windows flushes file data; it has no supported equivalent to Unix
directory fsync here.

Before replacement, a mismatch returns `Conflict` with current content and leaves
the destination untouched. A detected race or failure after replacement returns
`Uncertain`, with a durable recovery id. Post-replacement checks compare both the
saved bytes and the displaced bytes. `recovery_entries`, `recovery` and explicit
`discard_recovery` let an authorized owner reconcile and acknowledge these
records. Confirmed saves receive a separate durable marker only after both checks pass.
Their displaced inode is kept for `LibraryConfig::confirmed_save_grace` (five
minutes by default, independent of trash retention). It is not a recovery anomaly
during that window. At expiry, the adapter re-reads the displaced bytes: an
unchanged base is unlinked and its private record removed; changed bytes and
unconfirmed records stay for explicit review. A detected discrepancy remains
unresolved even if the bytes later return to base. Inaccessible or unregistered
roots are never swept using stored paths alone. Startup root registration,
`purge_trash`, recovery listing, and a once-per-minute background sweep perform
the same cleanup. A durable cleanup marker allows interrupted deletion to resume.
Legacy records without confirmation remain unresolved: matching bytes now cannot
prove that their original outcome was confirmed. Integrators must expose explicit
cleanup and retain the phone draft on uncertainty.

This is **not atomic compare-and-swap against arbitrary external writers**.
An external writer can still modify an already-open old inode after the final
check; retaining that displaced inode preserves writes observed at the end of
the grace window. Writes after its final check/unlink cannot be preserved; the
window is a bounded detection opportunity, not coordination with native writers. A native rename racing after the last observation can supersede a save.
Filesystem reads are snapshots, not transactions. A hostile local process able
to rename root ancestors, change mount topology or move an opened directory out
of a root can race containment rechecks. Windows replacement additionally uses
an absolute path after parent-handle validation. Root owners and native storage
are trusted; this is not an OS sandbox against a hostile same-user process.
Network filesystem stalls cannot be forcibly cancelled by the portable APIs.

## Trash, search and watching

Trash records are durable before removal. The source moves to an opaque staging
name, then moves into the configured data directory. On the same filesystem,
trash and restore retain the inode, including writes through an existing handle.
Across filesystems they use flushed, revision-checked copies; writes through an
old handle after the final copy check cannot be carried across that boundary. Completed records survive restart, preserve permissions,
and restore via atomic no-clobber publication. An incomplete operation reports
`OutcomeUnknown` and retains its stage/record for owner reconciliation; it is not
automatically retried or purged. `purge_trash` removes only completed records older
than the injected-clock retention period. No operating-system trash is assumed.
Rename uses no-clobber native rename on Linux and hard-link publication plus
unlink on Windows; the latter can leave two names after a crash. Mutations that
cannot confirm their final state return `OutcomeUnknown`.

Search uses bounded handle-based traversal, nested `.gitignore`/`.ignore` matchers
from `ignore`, a result cap of 1000, an entry limit, depth limit and clock deadline.
It skips `.git`, `node_modules`, `target`, large and binary files. A deadline or
entry limit returns results collected so far; the current port has no completeness
field. Ignore files are themselves read through validated handles, never ambient
Git configuration. The deadline bounds traversal work, not a blocked kernel I/O.

`notify` events are coalesced over 80 ms. Hashing happens only when subscribers
exist. Consumers must reload snapshots after broadcast lag, root changes or
`rescan_required()`, which records native errors, inaccessible changes and queue
overflow. Watch notifications are hints and can repeat adapter-generated events;
current content/revision remains authoritative. Directory-only changes require
relisting. No file contents, terminal input, screenshots or credentials are logged.
