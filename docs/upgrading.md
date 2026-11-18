# Updating and restoring an earlier version

Øphio doesn't download new versions of itself: you obtain each new package
from the same trusted source as the installed one. It tells you when one is
out ([below](#finding-out-about-new-versions)), installs them for you, and
keeps the agents it runs up to date
([below](#keeping-claude-code-and-codex-up-to-date)). Settings → Versions shows
the app version, installer when Android reports a recognized store, the
version of Øphio on the computer, a newer version when there is one, and the
negotiated protocol.

On Linux, `ophio update <download>` installs a new version: it waits until
nothing is running, restarts Øphio on the new version, checks that it came back
working and goes back to the earlier version if it didn't. It keeps only that
one earlier version and removes everything else an update unpacks, so updates
don't pile up on the disk. `packaging/linux/INSTALL.md` describes each step;
`ophio update --rollback` goes back.

Windows has an experimental unsigned download. The
experimental Mac download updates as Linux does, with `ophio update` and the
new archive; see [the Mac install guide](../packaging/macos/INSTALL.md#updating).
For a Windows per-user installation, extract the new ZIP into a new folder,
then run the stable launcher in `%LOCALAPPDATA%\Programs\Ophio`:

```powershell
.\ophio.exe update "C:\path\to\extracted-directory"
```

The updater copies what it needs and waits until Øphio has been idle for one
minute. Add `--now` to stop Øphio and update without that wait. Use the same
launcher with `update` alone to check status, `update --cancel` to cancel
before replacement begins, or `update --rollback` to restore the retained
previous version. Do not replace installed executables by hand. Recovery
uses the recorded helper after an interrupted update, including one interrupted
before replacement, and checks that the expected version starts. Keep its
recovery files if an executable stays locked, and retry a command when the
launcher asks. See [Windows installation and recovery](../packaging/windows/INSTALL.md).
Native recovery tests are not complete packaged install-and-update qualification;
see [the remaining limits](qualification/windows-macos.md).

A computer whose download carries the phone app offers it to its paired
Android phones, which update themselves over their connection to it, on the
same network or over Tailscale, without a cable or wireless debugging. The
phone shows a notification, and Settings leads with an **Update now** card
(also under Settings → Versions).
Android installs the update only when it is signed with the same key as the
installed app. Update a store-installed app through its store.

A connection requires a protocol version supported by both installations. An
incompatible connection leaves remote actions unavailable; phone drafts, Inbox
items and downloaded files remain local. The connection screen says **An update
is needed** and offers **Check updates**, which opens Settings → Versions instead
of retrying the same incompatible connection. Update the side named by the connection
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
a restart require review and are never automatically sent again. A chat whose
reply a restart cut off is the one exception to the review, and only when the
computer confirms the agent and what it started under Øphio have stopped: the person's next
message carries on in the same session. Confirmed local helpers are recorded as
stopped too; helpers of unknown lifetime or outside confirmed containment still
need review. A chat ready to carry on has no outcome-decision alert or demand to
mark it failed or completed. It tells you once, using your ordinary Chat reply
notification settings: “Reply cut off. Write to carry on.” Opening it goes to
the Chat; seeing the Chat clears it, and later restarts do not repeat it. Nothing
is left pending on Home. Replies already carried on before updating do not
raise this notice. Its receipt keeps the changes and steps, and says
when the person carried on. The cut-off reply is still not sent again.

Words reach the phone immediately and are saved on the computer about once a
second, and before a reply ends or a message interrupts it. A normal shutdown
saves the remaining words. A sudden crash can lose roughly the last second, or
more if the disk cannot save it. If a restart cuts a reply off, saved words and
recorded steps stay in the Chat, with the existing quiet **Cut off** line. Steps
from helpers do not split a reply; they can appear before the saved cut-off text.
Reopening the Chat or restarting again does not add another copy. This does not mean an
unfinished step succeeded. A task made from the Chat includes partial replies
in its conversation context, labelled as partial. Words lost during a restart
on an older version cannot be recovered by this update.

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

## Updating to 0.3.0

The final 0.3.0 Android and Linux files passed upgrade, phone self-update,
computer rollback and re-upgrade checks with an Android 15 emulator and an
Ubuntu 24.04.5 virtual machine. Chat replies and completed Tasks worked with
one end on 0.2.0 in both directions, using synthetic agents rather than real
providers. Pairing and observed history survived; a complete audit of every
setting and a second project was not run. Physical phones remain unqualified.
Keep a protected backup before updating. See the
[release-file results](qualification/linux.md#030-release-file-checks).

Update the computer first, with `ophio update
ophio-0.3.0-x86_64-linux.tar.gz`. Once it runs 0.3.0 it offers the new app to
its paired Android phones, and **Settings → Versions → Update now** installs
it over the phone's connection to the computer; installing
`ophio-0.3.0-arm64.apk` from the release by hand does the same. The app is
signed with the same release key as 0.2.0, so Android keeps its pairings and
settings. If Android says the signature doesn't match, stop: never uninstall
the app to get past it. Android may ask you to allow Øphio to install apps if
you turned that off after the last update, as [the phone's first
update](#the-phones-first-update-from-010) describes. If the first try just
after allowing installs from Øphio fails, tap **Try again**. That retry was
needed in the emulator when updating from 0.2.0; it does not describe a later
update started by 0.3.0.

0.3.0 doesn't change the computer's database format, so the update doesn't
need to back up or convert your data. The default for keeping Claude Code and
Codex up to date is now **Tell me when there's an update**. A computer that had
automatic agent updates on keeps them; one that had them off, or never chose,
now gets told about updates instead. See [Agent updates](agent-updates.md).

### While the phone and the computer run different versions

Most of what is new needs both ends on 0.3.0. Each end leaves out what the
other can't do rather than failing, but some things are missing until both
are updated.

**A 0.2.0 phone with a 0.3.0 computer:**

- Chats can't show attached files, and a suggested-work card can't be started
  as work linked to its chat.
- The phone can't set up or change a project's checks. Checks already approved
  still run after each task, and the task stays Working, with its **Stop**
  button, until they finish.
- **Settings › Agent apps** shows the old on-off switch, which can't show the
  new **Tell me** or **Don't check** choices.
- A notice about a new agent version may open a screen with nothing useful on
  it. The phone's own update offer works, so update the phone.

**A 0.3.0 phone with a 0.2.0 computer:**

- The phone hides what the computer can't do: switching agents in a chat,
  attaching files, suggested-work sets and automatic start, checks, and the
  three agent-update choices. **Settings › Agent apps** shows the computer's
  old switch.
- Tasks that are Watching say so in neutral words, without the newer
  explanation of what they wait for.
- Review drafts and **Run all** cards kept from a 0.3.0 computer can lose their
  link to the chat, so no summary comes back to it. Check the task itself for
  its result.

The single chat list across all projects and split view, on a tablet or an
unfolded foldable, are part of the phone app alone and work with either
computer version.

## Going back to 0.2.0

0.3.0 doesn't change the database format, so `ophio update --rollback`
restores the computer's previous version, as for any update. It doesn't put
the 0.2.0 app back on the phone, and it doesn't undo anything agents did in
your projects. The phone keeps the 0.3.0 app, which works with a 0.2.0
computer as described above. Rollback and re-upgrade were checked with the
final Linux archive and Android emulator, with pairing and observed history
retained. The older version's settings behavior below also has source tests;
this is not a physical-phone or complete settings-retention result.

What stays:

- Tasks, chats and history.
- Finished or skipped setup stays finished. `ophio setup` doesn't ask its
  Agents question again.
- Øphio's own copies of Claude Code and Codex stay and stay in use, whichever
  of the three update choices you had.

What changes:

- **Automatic agent updates start again.** 0.2.0 has only an on-off switch,
  and its "off" removes Øphio's copies, so 0.3.0 saves every choice as "on".
  0.2.0 checks for agent updates as soon as it starts and every six hours
  after, and installs them, even if you chose **Tell me** or **Don't check**.
  To stop that on 0.2.0, turn its switch off, knowing that this removes the
  copies and sends tasks back to the computer's own Claude Code and Codex.
- **The three-way choice is lost.** 0.2.0's first check rewrites the saved
  setting without it, so when you update to 0.3.0 again the setting is
  **Update automatically**. Choose **Tell me** or **Don't check** again in
  Settings › Agent apps if that is what you want.
- **The previous-agent selection is removed.** Published 0.2.0 removes the
  record of which older managed-agent version to fall back to. The current
  selection and saved program files stayed in the lab, but updating back to
  0.3.0 did not recreate the missing record. There is no recorded previous
  version to fall back to until a successful agent update creates one.
- **A failed agent update can remove a copy.** 0.3.0 keeps the working copy
  when an update fails; 0.2.0 doesn't have that fix.
- **Checks don't run.** 0.2.0 doesn't know about a project's checks, so tasks
  finish without them. They run again after you update to 0.3.0.
- **Some records show as unknown.** 0.2.0 can't read records of things only
  0.3.0 does. As with earlier versions, it keeps them, never repeats them, and
  lists their outcome as unknown, as
  [above](#updating-and-restoring-an-earlier-version) describes. If you know
  they finished, there is nothing to do.

After going back, and again after updating to 0.3.0, check that the phone is
still paired, that tasks show the results you expect, and that the agent
update setting is the one you want.

## Updating to 0.2.0

Øphio 0.2.0 is the first update with a new phone app since 0.1.0. Update the
computer first, with `ophio update ophio-0.2.0-x86_64-linux.tar.gz`. Once it
runs 0.2.0 it offers the new app to its paired Android phones, as described
above, and each phone updates over its connection to it; installing
`ophio-0.2.0-arm64.apk` from the release by hand does the same. The new app
is signed with the same release key as the 0.1.0 app, so Android installs it
over the old one and keeps its pairings and settings. Until a computer is
updated, the new app leaves out what that computer can't do yet and says to
update it where that matters.

0.2.0 chooses notifications by type instead of by topic. When it first
starts, the computer carries the earlier topic switches over to the new
choices, as [Notifications](notifications.md) describes, and then no longer
uses them. Finished tasks no longer alert unless you turn them on, or ring
the bell on a task to hear when that one is done. The computer's database
format is the same as in 0.1.1, so the update doesn't need to back up or
convert your data.

### The phone's first update from 0.1.0

This is the first time the phone app updates itself, and Android asks more
than the 0.1.0 app's screen says:

1. Android says Øphio isn't allowed to install unknown apps from this
   source. Tap **Settings**, turn on **Allow from this source** for Øphio,
   and go back. The exact words vary between Android versions and phone
   makers.
2. If the app then says "The update was cancelled.", tap **Try again**.
3. If you installed the app from your browser, Android may also ask "Do you
   want to update this app?". Tap **Update**.

The app closes while it installs and keeps its pairings, history and
settings. Open it again from the "Øphio updated" notification, or from
the app drawer if notifications are off. Later
updates ask less: the permission stays on, and the new app asks for it
before it starts an update rather than part way through.

The phone needs the File transfers permission to download the update.
Setup's recommended **Everything** includes it. A phone paired with fewer
permissions says "This device needs the File transfers permission to
download the update"; give it with `ophio devices allow "<device name>"
transfers` on the computer, or install the app by hand.

### Going back to 0.1.1

0.2.0 doesn't change the database, so `ophio update --rollback` is all going
back needs. Tasks, chats and history stay as they are. The phone keeps the
0.2.0 app, which works with a 0.1.1 computer and leaves out what it can't
do.

**Before going back, check pull requests you chose to merge yourself.** 0.1.1
doesn't know a task's own **When its checks pass** choice. In a project with
**Merge once checks pass** on, it merges every task's pull request once its
checks pass, including those set to **Leave it for me**. Merge or close them
first, or turn the project's switch off.

Notification choices don't go back with it, because 0.1.1 doesn't know
about notification types:

- On 0.1.1, the computer alerts about every topic again, including
  finished tasks and any you had turned off before updating, until you turn
  them off again in 0.1.1's settings. Muted projects stay muted.
- Your 0.2.0 choices are kept and come back when you update to 0.2.0 again,
  unless you change notification settings while on 0.1.1. Saving them
  there replaces the 0.2.0 choices with 0.1.1's switches, which the next
  update carries over again.

Other things 0.1.1 doesn't know about:

- Phones show the computer's own hostname again. The name you chose comes
  back when you update to 0.2.0 again.
- The bell on a task has no effect, and a task's own merge choice and bell
  are lost when 0.1.1 changes that task.
- A task looking into a problem report gets only its one-line first message
  if it starts a new session on 0.1.1, so finish or archive those first.
- If an agent was signed out, the phone's notice about it can't be cleared
  on 0.1.1: it stays in the task list without its **Sign in again** and
  **Not now** buttons, which come back when you update to 0.2.0 again.
- 0.1.1 can't read the records of some things only 0.2.0 does: renaming the
  computer, the bell, Switch to Work, Look into it, a task's merge choice,
  and signing an agent in from the phone. When 0.1.1 first starts it keeps
  those records, never repeats them, and lists their outcome as unknown, as
  [above](#updating-and-restoring-an-earlier-version) describes. If you know
  they finished, there is nothing to do.

## Finding out about new versions

Once a day, Øphio on the computer asks GitHub whether a newer Øphio has been
released. When one has, your paired phones get one notification, "Øphio
0.3.0 is available", and **Settings → Versions** shows the new version, a
**What's new** button that opens its release page in the phone's browser, and
how to update that computer. On the computer, `ophio update` with nothing
after it says the same, and `ophio update --json` includes it under
`release_check`. Nothing is downloaded or installed until you update.

- **What is checked.** The latest release of the public `Ophio-app/ophio`
  repository on GitHub, as GitHub's "latest release" gives it, which leaves
  out drafts and pre-releases. Only a stable version newer than the one on the
  computer counts. An answer that names no stable version, such as a tag
  that isn't a version number or a repository with no release, counts as no
  answer: a newer version already found stays.
- **How often.** The first check comes between 2 and 20 minutes after
  Øphio starts, then one a day, each up to 20 minutes late so computers don't
  all ask at once. Restarting Øphio doesn't ask again sooner, and a computer
  that sleeps much of the day still checks once a day while awake. Turning
  the check on from off asks at once. A check that gets no answer, for example because
  the computer is offline, says nothing and is tried again about six hours
  later.
- **What is sent.** One ordinary web request to `api.github.com`, without
  signing in. A GitHub account or token you gave Øphio is never used for it,
  and no cookies are kept. The request names the program only as `ophio`: not
  its version, the computer's name or anything about you or your projects. As
  with any web request, GitHub sees the computer's internet address.
- **Who is told.** The notification goes to the phones and tablets with the
  Tasks permission, as other notifications do. Every paired device sees the
  newer version in Settings → Versions.
- **Each computer checks for itself,** since each needs its own update, so
  with two computers you may be told twice about the same version, once for
  each. A version is told once per computer: swiping the notification away
  or opening it on one phone clears it from your other phones too, and it
  doesn't come back after a restart, an update of the app or turning the
  check off and on. A later version is told again. Once the computer runs
  that version or a newer one, the notice clears. The notification's type is
  **New Øphio versions** in Settings → Notifications, Silent by default; see
  [Notifications](notifications.md#what-notifies-you).

For the supported Linux download, run `ophio update` with the new archive,
as [above](#updating-and-restoring-an-earlier-version). The experimental Mac
download updates the same way. Windows has an experimental unsigned download.
Its per-user installations use the stable launcher with an extracted update
directory, not manual executable replacement. Settings → Versions shows
instructions for the computer's platform.

**Turning it off.** **Settings → Versions → Check for new versions of Øphio**
turns the check on or off for that computer. The choice is kept on the
computer, so it holds for every phone paired with it, and changing it needs
the Manage access permission. Off, the computer sends no requests, any
notice about a newer version clears, and what earlier checks found is
forgotten, so turned on again nothing is called the newest until a check
answers. A change shows at once on every phone
with Versions open, and so does a check under way. Builds made from the source with
`OPHIO_PERSONAL_SIGNING=1` start with it off, since they are built from the
newest source already; turning it on works the same there.

A computer whose Øphio predates this check shows nothing new in Versions, and
a phone app that predates it isn't sent the notice.

A fork that publishes its own releases points the check at its repository
with the `REPOSITORY` constant in `crates/companion/src/release_check.rs`.

## Keeping Claude Code and Codex up to date

Øphio can keep its own copies of Claude Code and Codex at their makers' newest
releases, so new models show up without anyone updating the computer. One
setting, in `ophio setup`'s Agents step and in **Settings › Agent apps** in the
app, offers three choices: **Update automatically**, **Tell me when there's an
update** (the default) and **Don't check**. Changing it from the app needs the
Manage access permission. [Agent updates](agent-updates.md) describes each
choice, the **Update now** button and the notices about new models.

An update downloads the newest release's program for this computer's system
from the npm registry, where Anthropic and OpenAI publish them; checks it
against the checksum the registry publishes; and starts it once with
`--version`. Only a copy that passes all three is used, from the next task on.
If an update fails, the copy already in use stays. Tasks already running keep
the version they started with. The settings card shows each copy's version,
when it was last checked, and why a check or download didn't finish.

Tasks and terminals Øphio starts then use its copies. The Claude Code and Codex
installed on the computer stay as they are, with the same sign-in, settings
and history; Øphio's copy of Claude Code starts with its own updater off so it
never installs over the computer's. The copies live in `agents/` in Øphio's
data directory and take up to about 1.5 GB. None of the three choices removes
them.

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
