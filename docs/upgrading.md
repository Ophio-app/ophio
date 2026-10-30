# Updating and restoring an earlier version

Øphio doesn't look for or download new versions of itself: you obtain each
new package from the same trusted source as the installed one. It installs
them for you, though, and keeps the agents it runs up to date
([below](#keeping-claude-code-and-codex-up-to-date)). Settings → Versions shows
the app version, installer when Android reports a recognized store, the
version of Øphio on the computer and the negotiated protocol.

On Linux, `ophio update <download>` installs a new version: it waits until
nothing is running, restarts Øphio on the new version, checks that it came back
working and goes back to the earlier version if it didn't. It keeps only that
one earlier version and removes everything else an update unpacks, so updates
don't pile up on the disk. `packaging/linux/INSTALL.md` describes each step;
`ophio update --rollback` goes back. On Windows, replace `ophio.exe` and
`computer-mcp.exe` with the new download's while Øphio is stopped.

A computer whose download carries the phone app offers it to its paired
Android phones, which update themselves over their connection to it, on the
same network or over Tailscale, without a cable or wireless debugging. The
phone shows a notification, and Settings leads with an **Update now** card
(also under Settings → Versions).
Android installs the update only when it is signed with the same key as the
installed app. Update a store-installed app through its store.

A connection requires a protocol version supported by both installations. An
incompatible connection leaves remote actions unavailable; phone drafts, Inbox
items and downloaded files remain local. Update the side named by the connection
error. Older versions on the computer may not report their supported versions;
in that case compare the two packages from their original source before
updating.

`ophio update` waits for active work to finish before it stops Øphio. Keep a
backup of Øphio's data directory and configuration, with the same access
protections as the originals, before a version that may change the database.
After updating by hand, start Øphio and confirm the paired identity and access
grants before running work.

Øphio on the computer stores its journal, projection records and command
outcomes in SQLite. Before each schema migration it creates a backup beside the
database: `<database-stem>.before-v<destination-schema>-<id>.sqlite`. For
example, migrating `companion.sqlite` to schema 1 creates
`companion.before-v1-<id>.sqlite`. The backup includes committed WAL pages. A
successful transaction changes the schema; a failed transaction does not leave a
partially applied migration. Added optional record fields can load with their
specified defaults without a schema migration.

Stored commands from older releases remain readable even when a newer app must
send additional safety information. For example, old permission-change history
may lack the editor's expected grants; a new request without that baseline is
refused with an instruction to update the app. Pending commands recovered after
a restart require review and are never automatically sent again.

If an individual stored command cannot be decoded, Øphio keeps its
original bytes, permanently reserves its command ID, and records an unknown
outcome requiring reconciliation. The service log includes the command ID and
a decode diagnosis without the stored values. Managers can see the unknown
outcome in the journal; retries of that ID are refused. Review the actual effect
before issuing a new command. A malformed ID stays reserved locally but cannot
be represented as a journal command ID.

Startup still refuses unreadable authority/projection documents or database
failures: skipping permission, task, or approval state could allow unsafe work,
and a failed recovery transaction cannot durably record an unknown outcome.
Journal decoding also remains strict; skipping journal entries could conceal
effects. Startup diagnostics are written to the service log as well as printed
by `ophio run`. Release compatibility fixtures and their refresh procedure
are in `crates/store/tests/compatibility.rs`.

`ophio update --rollback` puts the earlier programs back and restarts Øphio,
which is all going back needs when the newer version didn't change the
database. When it did, the earlier version refuses the database: the rollback
says it didn't come back working and leaves the newer version running. Then go
back by hand:

1. Stop Øphio and confirm it is no longer running. Preserve the current
   database, its `-wal` and `-shm` files, configuration and the migration backups in
   a separate protected directory.
2. Run `ophio update --rollback --now`, or reinstall the previous Øphio package
   from the same trusted source.
3. Restore the backup made **before the first migration that the previous version
   cannot read**, using the database's original filename. Move the newer database
   and its WAL/SHM files aside together; do not leave a newer WAL next to the
   restored database. Keep the backups until recovery is verified.
4. Start the previous version and check its identity, grants and records. An
   older version refuses a newer schema instead of attempting a downgrade. If it
   still refuses, use a compatible backup or reinstall the newer version.

Restoring a database loses records on the computer written after that backup. It
does not undo project files, terminal commands, agent work, transfers already
written to disk, or changes to outside applications. It does not roll back the
Android app, phone drafts, downloads or native agent accounts. Pairing and
revocation records also return to their backup state: review device/channel
access before reconnecting. Never assume a restored record proves an external
action was undone.

## Keeping Claude Code and Codex up to date

Øphio can keep its own copies of Claude Code and Codex at their makers' newest
releases, so new models show up without anyone updating the computer. It is
off until you turn it on: `ophio setup` asks once in its Agents step and
recommends it, and **Settings › Agent apps › Keep Claude Code and
Codex up to date** in the app turns it on or off. Changing it from the app needs
the Manage access permission.

While it is on, Øphio checks when it starts and every six hours. For each agent
installed on the computer it downloads the newest release's program for this
computer's system from the npm registry, where Anthropic and OpenAI publish
them; checks it against the checksum the registry publishes; and starts it once
with `--version`. Only a copy that passes all three is used, from the next task
on. Each check starts the copy in use the same way; one that no longer starts
is set aside, so tasks use the computer's program until a new copy replaces
it. Tasks already running keep the version they started with, which stays
until the following update. The settings card shows each copy's version, when
it was last checked, and why a check or download didn't finish.

Tasks and terminals Øphio starts then use its copies. The Claude Code and Codex
installed on the computer stay as they are, with the same sign-in, settings
and history; Øphio's copy of Claude Code starts with its own updater off so it
never installs over the computer's. The copies live in `agents/` in Øphio's
data directory and take up to about 1.5 GB. Turning the setting off sends the
next tasks back to the computer's programs and removes the copies; the one
tasks were last using stays for those still running until the next check or
restart.

## Upgrading from the earlier companion program

Before the product was named Øphio, the program on the computer was called
`companion`. Upgrading keeps the computer's settings, identity, paired devices
and history:

- On Linux, run the new archive's `install.sh`, then `ophio service install`.
  `packaging/linux/INSTALL.md` lists the steps.
- On Windows, stop the earlier `companion.exe` before running any `ophio.exe`
  command: close its window, or end it in Task Manager. Øphio's commands
  cannot reach the earlier program while it runs, and its folders cannot move
  until it stops. Then run `.\ophio.exe service install` from the new
  directory; it starts Øphio, replaces the logon registration the earlier
  program made and leaves any other alone.
- When Øphio starts, it renames the earlier folders to its own:
  `~/.config/companion` and `~/.local/share/companion` on Linux,
  `%APPDATA%\companion` and `%LOCALAPPDATA%\companion` on Windows. A folder
  that already exists under the new name is never merged into or replaced.
- If the earlier program is still running, or a folder cannot be renamed, for
  example because the new name is on another filesystem, Øphio says what
  happened and keeps using the old folder. Stop the earlier program or fix the
  cause, then start Øphio again.
- The Øphio app for Android has a new app ID, so it installs beside the earlier
  app instead of replacing it. Pair it with `ophio pair` as a new device, then
  remove the earlier app and end its access with `ophio devices revoke`.

To go back to the earlier program, stop Øphio and rename its folders back to
the earlier names before starting the earlier program.

## Private question answers

Answers to a question marked secret are sent to the waiting agent but withheld
from saved commands, approval delivery records and journal events. If a request
mixes ordinary and secret questions, the entire answer is withheld, including
its free-text summary. Command IDs and delivery outcomes remain available;
reconnecting or restarting never sends a saved password again. Once the agent
has moved on, a private answer requires a fresh question rather than becoming a
saved follow-up message.

On opening its database, the computer scrubs these answers from existing
command, approval and journal records without changing command IDs or journal
sequence numbers. Pending private follow-ups and their linked retained resume or queue commands
are replaced with a request to ask again. This cleanup does not rewrite separate backups, exported histories or
copies already received by an agent or another client. Restoring an old database
into the updated computer runs the cleanup again; running an older executable
can retain newly submitted answers again.

The shared phone client withholds secret answers from its command state and
filters approval replies and replay from older computers. Its offline cache
contains neither approvals nor command payloads. Updating only the phone does
not fix an older computer's storage or sharing; update the computer too. The
same shared client code applies to Android and iPhone.
