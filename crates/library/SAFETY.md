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
On Windows identity checks first request `FileIdInfo`: the full 128-bit file ID
and 64-bit volume serial, with the owning handle retained. Only unsupported-query
errors 1, 50 and 87 allow fallback to the complete legacy volume serial and file
index. Full and legacy identities never compare equal. Legacy identity always
retains its original strict capability pin, preventing rename and index reuse
while the root is registered. Other identity-query failures refuse the operation.
Only a full identity on a handle-confirmed local, non-removable NTFS or ReFS
volume may exchange its strict pin for a matching identity handle that permits
rename and deletion between operations. FAT, FAT32, exFAT, removable media,
network/unknown filesystems, and failed filesystem/device classification keep
the strict pin, so the root cannot be moved while registered. Native tests on
real FAT32 and exFAT volumes confirm unsupported full-ID error 87, legacy
registration and reads, strict rename refusal, unchanged target and recovery
trees after refused writes, and normal NTFS saves alongside. An authenticated
companion test also starts with FAT32 and NTFS roots already configured, then
lists and reads FAT32 and receives the plain save refusal through pinned TLS.
ReFS volume qualification remains outstanding.
Each operation opens a separate capability handle, verifies it against that
still-open identity pin, and retains its root rename protection through
child-directory operations. A replacement at the registered path is refused
until an explicit root refresh; returning the original directory preserves its
identity. The rename-friendly identity handle is never used for capability
operations. Windows still refuses to rename an ancestor of an open directory,
even with delete sharing. Therefore an independently registered nested root pins
moves of its parent; unregister the nested root first. Identity pinning is not
discarded to avoid this native limit.
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

Host-side Windows project-overlap, blocked-write association, received-file
containment and catalog-to-Library matching use the same comparison helper on
both paths. It resolves the longest existing ancestor, follows SUBST drives and
junctions, appends only missing normal components, and retains the extended
Windows spelling regardless of path length. Existing entries that cannot be
resolved, including dangling junctions, are refused rather than mistaken for
missing folders. This spelling is for comparisons, not display; ordinary paths
remain ordinary where that names the same place. Resolution is still a snapshot:
concurrent remounts or renames are not made transactional. The pre-existing
case-sensitive comparison of missing components is unchanged.

## Saves and recovery

Windows permits mutations only when the opened root volume reports both
`FILE_SUPPORTS_HARD_LINKS` and `FILE_PERSISTENT_ACLS`. A failed capability query
also refuses mutations. This checks actual reported capabilities, not the
filesystem name. FAT32 and exFAT remain available for registration, listing and
reading, but create, save and other writes return `Unsupported` with:
“This drive's format does not support the safe saving Ophio needs. Reading still
works.” The check runs before creating temporary files or private recovery
records. Automatic confirmed-save cleanup also skips unsupported roots. No
alternative publication path is introduced; Windows still requires its existing
hard-link publication and ACL-preservation behavior. Release 0.2 already required
hard links for publication and unconditionally copied Windows ACLs, so these
formats did not have working safe saves before this explicit early refusal.
Only the typed `UnsupportedVolume` error exposes its fixed, path-free explanation
to authenticated clients, with protocol code `Unsupported`; other Library errors
retain their generic client message.

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
`rescan_required()`, which records reported registration failures, native errors,
inaccessible changes and queue overflow. Root refresh acknowledges only requests
that preceded its registration; concurrent requests and failed refreshes remain
outstanding. Registrations remain batched and are rebuilt on every refresh on
all platforms, with no additional directory identity handles. The macOS `notify`
backend does not report every native stream-start failure, so successful
registration alone is not a readiness guarantee. Refresh latency remains an open
performance limitation; no cached registration is treated as proof of monitoring.

On Windows a full-access root uses one recursive native watch; other roots
covered by it may share that watch. A limited root instead watches its top folder
non-recursively and each existing granted subfolder separately, retaining the
grant's path spelling and an independent buffer per watched subfolder. A limited
root never subsumes a nested project's watch. Exact registration paths shared by
multiple grants are registered only once, using recursive coverage if any grant
requires it; notify's duplicate-path replacement would otherwise orphan handles.
A failed Windows registration does not skip later paths shared with other roots.
Slash-separated grants are appended one component at a time, including for long
verbatim paths and UNC paths. Missing grants remain eligible for a later refresh;
unexpected inspection failures produce one warning per registration attempt and
request a rescan rather than silently dropping monitoring.
Descendant watch handles can prevent moving a limited root or its ancestor on
Windows; this is intentional, rather than letting an unrelated busy tree consume
a project's native buffer. Full-access roots retain move support where the
filesystem identity policy permits it, unless an overlapping limited grant or
independently registered nested root still pins them.
A Windows refresh explicitly unwatches old registration paths and waits at most
two seconds for an ordered backend acknowledgment that their directory handles
have closed. Merely dropping notify's watcher only queues shutdown and does not
provide this boundary. Failed, dead or hung retirement marks a rescan and logs a
warning, but does not prevent installing the replacement roots and watcher. If
native cancellation never returns, the detached retirement task may retain the
old watcher and its handles until the backend recovers or the process exits; an
immediate folder move can therefore still fail. The retirement worker retains
paths, not root identity pins. This timeout bounds the retirement wait while
holding the Library state lock; it does not make other blocking filesystem or
watch-registration calls cancellable.
Unrelated sibling events are discarded before entering the coalescing queue or
reading file contents. Changes to ancestor folders of granted paths and any
reported rescan/error signals are retained. Reported callback errors request a
fresh snapshot and rebuild watches from the current grants, even without change
subscribers. Recovery attempts are limited to once per minute, keeping failed
requests pending so one persistently unwatchable grant cannot rebuild every
healthy watch on each incoming event batch.

The Windows backend in notify 8.2.0 does **not** deliver every native failure to
its callback: a success with zero bytes after buffer overflow can drop changes
silently, while some native errors log and remove a watch without emitting `Err`.
Its runtime callback currently emits only successful events, so error-recovery
coverage uses an injected reported error. Those silent native cases cannot be
detected or repaired through this callback API; reducing unrelated traffic is not
a guarantee against overflow or silent watch loss. No notify patch is included.
Other platforms retain their existing selective watches and batched registration.

Watch notifications are hints and can repeat adapter-generated events;
current content/revision remains authoritative. Directory-only changes require
relisting. No file contents, terminal input, screenshots or credentials are logged.
